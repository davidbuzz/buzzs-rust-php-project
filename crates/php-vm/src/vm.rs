// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The virtual machine: global state, frames and request lifecycle.

use crate::class::{Class, NameMap};
use crate::compiler::CompiledUnit;
use crate::func::{Func, UserFunction};
use crate::opcode::OpArray;
use php_runtime::{HashTable, Obj, PhpRef, Value, ZStr};
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
        // A recycled frame of the same function still shares them
        // (`Frame::clear_for_reuse`): no reference count to move.
        if !self.shared.as_ref().is_some_and(|c| Rc::ptr_eq(c, code)) {
            self.shared = Some(code.clone());
        }
        self.owned.clear();
    }

    /// Drops all names; a small owned buffer keeps its capacity for reuse.
    /// Inline, and a no-op on names already dropped: every recycled frame
    /// calls it on its return.
    #[inline]
    fn clear(&mut self) {
        clear_opt(&mut self.shared);
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

/// Records slot `slot` of compiled variable `cv` (of `ncv`) in a frame's
/// [`Frame::symtab_slots`]. The list stays empty (nothing allocated) while
/// every variable is at its own index, as in a function's own table; the
/// first one elsewhere fills in the ones before it.
#[inline]
fn record_slot(slots: &mut Vec<u32>, cv: usize, slot: u32, ncv: usize) {
    if slots.is_empty() {
        if slot as usize == cv {
            return;
        }
        slots.reserve(ncv);
        slots.extend((0..cv).map(|i| u32::try_from(i).unwrap_or(u32::MAX)));
    }
    slots.push(slot);
}

/// The attached table's slot of compiled variable `cv` named `name`: from
/// `slots` (the frame's [`Frame::symtab_slots`]) when they are this
/// table's, else by name.
#[inline]
fn mapped_slot(
    slots: &[u32],
    valid: bool,
    st: &mut VarStore,
    cv: usize,
    name: &ZStr,
) -> Option<u32> {
    match slots.get(cv) {
        Some(&s) if valid && (s as usize) < st.slots.len() => Some(s),
        _ => st.find_cv_z(cv, name),
    }
}

/// Whether `a` and `b` are the same handle (or equal scalars): a frame's
/// compiled variable that already holds its symbol-table slot's value.
#[inline]
fn same_handle(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => ZStr::ptr_eq(x, y),
        (Value::Array(x), Value::Array(y)) => php_runtime::ZArray::ptr_eq(x, y),
        (Value::Object(x), Value::Object(y)) => Obj::ptr_eq(x, y),
        (Value::Resource(x), Value::Resource(y)) => php_runtime::Resource::ptr_eq(x, y),
        (Value::Reference(x), Value::Reference(y)) => PhpRef::ptr_eq(x, y),
        _ => false,
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
    index: Option<NameMap<u32>>,
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

    /// Empties the table and gives it the compiled-variable names of
    /// `code` (a private table reused for the next walk, see
    /// `Vm::with_cv_symtab`).
    pub(crate) fn reset_for(&mut self, code: &Rc<OpArray>) {
        self.names.share(code);
        self.index = None;
        self.slots.clear();
        self.slots.resize(code.cv_names.len(), None);
    }

    fn build_index(&mut self) {
        if self.index.is_none() {
            let mut m = NameMap::default();
            m.reserve(self.names.len());
            for (i, n) in self.names.iter().enumerate() {
                m.insert(n.clone(), u32::try_from(i).unwrap_or(0));
            }
            self.index = Some(m);
        }
    }

    /// Finds a variable slot by name.
    pub fn find(&mut self, name: &[u8]) -> Option<u32> {
        let p = if self.names.len() < 8 {
            self.names
                .iter()
                .position(|n| n.as_bytes() == name)
                .map(|p| u32::try_from(p).unwrap_or(0))
        } else {
            self.build_index();
            self.index.as_ref().and_then(|m| m.get(name).copied())
        };
        if let Some(idx) = p {
            if self.slots.len() <= idx as usize {
                self.slots
                    .resize(self.names.len().max(idx as usize + 1), None);
            }
        }
        p
    }

    /// The slot of compiled variable `cv`, named `name`: `cv` itself when
    /// the table's names line up with the op array's there (every
    /// function's own table), else [`VarStore::find`] by name.
    #[inline]
    pub fn find_cv(&mut self, cv: usize, name: &[u8]) -> Option<u32> {
        if self.names.get(cv).is_some_and(|n| n.as_bytes() == name) {
            if self.slots.len() <= cv {
                self.slots.resize(self.names.len().max(cv + 1), None);
            }
            if let Ok(i) = u32::try_from(cv) {
                return Some(i);
            }
        }
        self.find(name)
    }

    /// [`VarStore::find_cv`] for a name that is usually the very string
    /// the table holds (the op array's own `cv_names`): compared by
    /// pointer before by bytes.
    #[inline]
    pub fn find_cv_z(&mut self, cv: usize, name: &ZStr) -> Option<u32> {
        if self
            .names
            .get(cv)
            .is_some_and(|n| ZStr::ptr_eq(n, name) || n.as_bytes() == name.as_bytes())
        {
            if self.slots.len() <= cv {
                self.slots.resize(self.names.len().max(cv + 1), None);
            }
            if let Ok(i) = u32::try_from(cv) {
                return Some(i);
            }
        }
        self.find(name.as_bytes())
    }

    /// [`VarStore::find_cv`], creating the slot when the name is missing.
    #[inline]
    pub fn find_or_add_cv(&mut self, cv: usize, name: &[u8]) -> u32 {
        match self.find_cv(cv, name) {
            Some(i) => i,
            None => self.find_or_add(name),
        }
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
    /// Fast inline argument buffer avoiding temporary Vec allocation.
    pub inline_args: [Option<Value>; 6],
    /// Count of inline arguments stored in `inline_args`.
    pub inline_count: u8,
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

/// How many argument records [`CallBufs::args_pool`] keeps: the depth of
/// nested builtin calls (a callback's builtin call inside `array_map()`)
/// served without allocating.
const ARGS_POOL: usize = 8;

/// Reusable buffers of the call protocol (see [`Vm::call_bufs`]).
#[derive(Debug, Default)]
pub(crate) struct CallBufs {
    /// Pending-call argument vectors.
    pub(crate) args: Vec<Vec<Option<Value>>>,
    /// ZPP value vectors.
    pub(crate) values: Vec<Vec<Value>>,
    /// ZPP passed-flag vectors.
    pub(crate) passed: Vec<Vec<bool>>,
    /// The value vector lent to the handler of the newest builtin call
    /// ([`CallBufs::take_values`]): taken and put back whole, three words
    /// each way, where a pool `pop` and an out-of-line `push` cost ~30
    /// instructions a call. Empty (no capacity) while lent; a nested call
    /// meanwhile takes from [`CallBufs::values`].
    pub(crate) lent_values: Vec<Value>,
    /// The passed-flag vector lent alongside [`CallBufs::lent_values`].
    pub(crate) lent_passed: Vec<bool>,
    /// Boxed argument records for the tape's internal call
    /// (`exec/tape/calls.rs`, `call_native`; [`CallBufs::take_args`]):
    /// one is taken whole for a call, one pointer each way, its two
    /// vectors keeping their capacity, where lending the vectors one by
    /// one and building the record on the stack cost ~105 instructions a
    /// call. A nested call takes the next one.
    pub(crate) args_pool: Vec<Box<crate::func::Args>>,
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
            args: Vec::with_capacity(128),
            values: Vec::with_capacity(16),
            passed: Vec::with_capacity(16),
            lent_values: Vec::with_capacity(8),
            lent_passed: Vec::with_capacity(8),
            args_pool: Vec::with_capacity(ARGS_POOL),
            user_frames: Vec::with_capacity(8),
            methods: crate::call::MethodCache::default(),
        };
        for _ in 0..4 {
            b.args.push(Vec::with_capacity(8));
            b.values.push(Vec::with_capacity(8));
            b.passed.push(Vec::with_capacity(8));
            b.user_frames.push(Frame::new_pooled(empty_code.clone()));
            b.args_pool.push(CallBufs::new_args());
        }
        b
    }

    /// An empty argument record for a builtin call, taken whole from the
    /// pool ([`CallBufs::args_pool`]); a new one when the calls in progress
    /// hold them all.
    #[inline]
    pub(crate) fn take_args(&mut self) -> Box<crate::func::Args> {
        self.args_pool.pop().unwrap_or_else(CallBufs::new_args)
    }

    /// Gives back a record [`CallBufs::take_args`] lent, emptied by the
    /// caller (`Args::release_in_place`).
    #[inline]
    pub(crate) fn put_args(&mut self, a: Box<crate::func::Args>) {
        debug_assert!(a.values.is_empty(), "the values were released");
        if self.args_pool.len() < ARGS_POOL {
            self.args_pool.push(a);
        } else {
            CallBufs::drop_args(a);
        }
    }

    /// A record with room for eight arguments. It outlives the request,
    /// as php-src's VM stack does: its memory is not the request's
    /// (`memory_get_usage()`).
    #[cold]
    #[inline(never)]
    fn new_args() -> Box<crate::func::Args> {
        php_memlimit::persistent(|| {
            Box::new(crate::func::Args {
                values: Vec::with_capacity(8),
                passed: Vec::with_capacity(8),
                ..crate::func::Args::default()
            })
        })
    }

    /// Releases a record the full pool has no room for.
    #[cold]
    #[inline(never)]
    fn drop_args(a: Box<crate::func::Args>) {
        php_memlimit::persistent(|| drop(a));
    }

    /// An empty value vector for a builtin's [`crate::func::Args`]: the
    /// lent one when it is home, else one from the pool (a nested call's,
    /// or the lent one was kept by a handler), else a new one.
    #[inline]
    pub(crate) fn take_values(&mut self) -> Vec<Value> {
        if self.lent_values.capacity() != 0 {
            std::mem::take(&mut self.lent_values)
        } else {
            self.values.pop().unwrap_or_default()
        }
    }

    /// An empty passed-flag vector, as [`CallBufs::take_values`].
    #[inline]
    pub(crate) fn take_passed(&mut self) -> Vec<bool> {
        if self.lent_passed.capacity() != 0 {
            std::mem::take(&mut self.lent_passed)
        } else {
            self.passed.pop().unwrap_or_default()
        }
    }

    /// Gives back a value vector [`CallBufs::take_values`] lent, emptied
    /// by the caller (its values released in their order): home when home
    /// is free, else to the pool, as `Vm::put_val_vec` returns one.
    #[inline]
    pub(crate) fn put_values(&mut self, v: Vec<Value>) {
        debug_assert!(v.is_empty(), "the values were released");
        let cap = v.capacity();
        if cap == 0 || cap > 256 {
            return;
        }
        if self.lent_values.capacity() == 0 {
            // The empty vector there owns nothing.
            std::mem::forget(std::mem::replace(&mut self.lent_values, v));
        } else if self.values.len() < 4 {
            self.values.push(v);
        }
    }

    /// Gives back a passed-flag vector, as [`CallBufs::put_values`].
    #[inline]
    pub(crate) fn put_passed(&mut self, mut v: Vec<bool>) {
        let cap = v.capacity();
        if cap == 0 || cap > 256 {
            return;
        }
        v.clear();
        if self.lent_passed.capacity() == 0 {
            std::mem::forget(std::mem::replace(&mut self.lent_passed, v));
        } else if self.passed.len() < 4 {
            self.passed.push(v);
        }
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
            inline_args: [None, None, None, None, None, None],
            inline_count: 0,
        }
    }

    /// Total number of arguments currently stored in this pending call.
    #[inline(always)]
    pub fn arg_count(&self) -> usize {
        if self.inline_count > 0 {
            self.inline_count as usize
        } else {
            self.args.len()
        }
    }

    /// Move any inline arguments into `args`.
    pub fn materialize_args(&mut self) {
        if self.inline_count > 0 {
            let n = self.inline_count as usize;
            self.args.reserve(n);
            for i in 0..n {
                self.args.push(self.inline_args[i].take());
            }
            self.inline_count = 0;
        }
    }

    /// Whether this is a magic `__call` or `__callStatic` trampoline.
    #[inline(always)]
    pub fn is_magic_trampoline(&self) -> bool {
        self.magic_name.is_some()
            && !self.func.is_closure()
            && !(self
                .magic_name
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(b"__invoke"))
                && (self.closure.is_some()
                    || self
                        .scope
                        .as_ref()
                        .is_some_and(|s| s.lc_name.as_bytes() == b"closure")))
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
use std::cell::UnsafeCell;

/// An inline compiled variable slot in a frame.
///
/// Guaranteed zero-overhead `Option<Value>` representation (16 bytes,
/// identical to `Option<Value>`), so the tape interpreter and the JIT can
/// address slots by offset. Safe code reads through `&self` and writes
/// only through `&mut self`, so a reference from [`CvSlot::get`] can never
/// outlive the value it points to: a store while one is held (a
/// destructor or `__set` run by user code replacing the variable) does
/// not compile. The only shared-reference write is the `unsafe`
/// [`CvSlot::get_mut_unchecked`], whose callers must hold no such
/// reference.
#[repr(transparent)]
pub struct CvSlot(UnsafeCell<Option<Value>>);

impl CvSlot {
    #[inline(always)]
    pub fn new(val: Option<Value>) -> Self {
        Self(UnsafeCell::new(val))
    }

    #[inline(always)]
    pub fn get(&self) -> Option<&Value> {
        // SAFETY: writes need `&mut self` (or the `unsafe`
        // `get_mut_unchecked`, whose contract excludes live shared
        // references), so no write can happen while this borrow lives.
        unsafe { (*self.0.get()).as_ref() }
    }

    #[inline(always)]
    pub fn get_clone(&self) -> Option<Value> {
        self.get().cloned()
    }

    /// Stores `val`, returning the previous value (whose drop may run a
    /// destructor: callers drop it once they no longer borrow the frame).
    #[inline(always)]
    pub fn set(&mut self, val: Option<Value>) -> Option<Value> {
        std::mem::replace(self.0.get_mut(), val)
    }

    /// A mutable view through a shared reference, for the paths that hold
    /// the frame only by `&` (the tape interpreter's raw slot pointers).
    ///
    /// # Safety
    ///
    /// No reference from [`CvSlot::get`] (or another
    /// `get_mut_unchecked`) to this slot may be alive while the returned
    /// one is used.
    #[inline(always)]
    pub unsafe fn get_mut_unchecked(&self) -> &mut Option<Value> {
        // SAFETY: the caller guarantees exclusive access (see above).
        unsafe { &mut *self.0.get() }
    }

    #[inline(always)]
    pub fn as_mut(&mut self) -> &mut Option<Value> {
        self.0.get_mut()
    }

    #[inline(always)]
    pub fn is_none(&self) -> bool {
        self.get().is_none()
    }

    #[inline(always)]
    pub fn is_some(&self) -> bool {
        self.get().is_some()
    }
}

impl Clone for CvSlot {
    fn clone(&self) -> Self {
        Self::new(self.get_clone())
    }
}

impl std::fmt::Debug for CvSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(f)
    }
}

/// Contiguous execution stack for call frames.
///
/// Frames live in place on a contiguous vector (`vec[..live]`).
/// Released frames in `vec[live..]` are retained in place (up to 8) as the pool,
/// avoiding all frame moves between stack and pool.
#[derive(Debug, Default)]
pub struct FrameStack {
    pub(crate) vec: Vec<Frame>,
    pub(crate) live: usize,
}

impl FrameStack {
    pub fn new() -> Self {
        Self {
            vec: Vec::with_capacity(128),
            live: 0,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            vec: Vec::with_capacity(cap),
            live: 0,
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.live
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[Frame] {
        &self.vec[..self.live]
    }

    #[inline(always)]
    pub fn as_mut_slice(&mut self) -> &mut [Frame] {
        &mut self.vec[..self.live]
    }

    #[inline]
    pub fn push(&mut self, frame: Frame) {
        if self.live < self.vec.len() {
            self.vec[self.live] = frame;
        } else {
            self.vec.push(frame);
        }
        self.live += 1;
    }

    #[inline]
    pub fn pop(&mut self) -> Option<Frame> {
        if self.live == 0 {
            return None;
        }
        self.live -= 1;
        Some(self.vec.remove(self.live))
    }

    /// Pushes a boxed frame (a generator's) by swapping it with the
    /// released frame kept in the slot for the next call at this depth:
    /// nothing is dropped or freed, and the box comes back holding that
    /// frame, for [`FrameStack::pop_boxed`] to swap back. With no frame
    /// kept there, the frame moves in as [`FrameStack::push`] moves it, the
    /// box is freed and `None` is returned.
    #[inline]
    pub(crate) fn push_boxed(&mut self, mut frame: Box<Frame>) -> Option<Box<Frame>> {
        if let Some(slot) = self.vec.get_mut(self.live) {
            std::mem::swap(slot, &mut *frame);
            self.live += 1;
            return Some(frame);
        }
        self.vec.push(*frame);
        self.live += 1;
        None
    }

    /// Pops the top frame into a box: into `spare` (the box
    /// [`FrameStack::push_boxed`] gave back) by a swap that puts the
    /// released frame it holds back in the slot, else into a new box,
    /// leaving an empty frame in the slot for the next push to swap with.
    /// The frames kept above stay where they are. `None` when the stack is
    /// empty.
    #[inline]
    pub(crate) fn pop_boxed(
        &mut self,
        spare: Option<Box<Frame>>,
        empty_code: &Rc<OpArray>,
        empty_vars: &Rc<RefCell<VarStore>>,
    ) -> Option<Box<Frame>> {
        let top = self.live.checked_sub(1)?;
        let slot = self.vec.get_mut(top)?;
        self.live = top;
        Some(match spare {
            Some(mut b) => {
                std::mem::swap(slot, &mut *b);
                b
            }
            None => Box::new(std::mem::replace(
                slot,
                Frame::empty_builtin(empty_code, empty_vars),
            )),
        })
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len < self.live {
            self.live = len;
            self.vec.truncate(len);
        }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.live = 0;
        self.vec.clear();
    }

    #[inline]
    pub fn last(&self) -> Option<&Frame> {
        self.as_slice().last()
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut Frame> {
        self.as_mut_slice().last_mut()
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<&Frame> {
        if index < self.live {
            self.vec.get(index)
        } else {
            None
        }
    }

    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Frame> {
        if index < self.live {
            self.vec.get_mut(index)
        } else {
            None
        }
    }

    /// Reinitializes and advances the callee frame at `self.live` in place.
    #[inline]
    pub fn push_in_place(&mut self, code: &Rc<OpArray>, kind: FrameKind) -> usize {
        let idx = self.live;
        if idx < self.vec.len() {
            let f = &mut self.vec[idx];
            if f.cvs.len() == code.cv_names.len()
                && f.tmps.len() == code.num_tmps as usize
                && f.iters.len() == code.num_iters as usize
                && f.fins.len() == code.num_finally as usize
            {
                f.reinit_same_call(code, kind);
            } else {
                f.reinit_for_call(code, kind);
            }
        } else {
            self.vec.push(Frame::new(code.clone(), kind));
        }
        self.live += 1;
        idx
    }

    /// Pushes the frame of a builtin call (`zend_execute_internal`'s small
    /// call frame on the VM stack) and returns it, for the caller to set
    /// the call's parts: the function, arguments, `$this`, scopes and names.
    ///
    /// A pooled frame at this depth is readied in place
    /// ([`Frame::reinit_for_builtin`]): its buffers stay with it for the
    /// next user call made at this depth, and no `Frame` is built and
    /// copied. Otherwise an empty frame for `empty_code` sharing
    /// `empty_vars` is appended. Every field the caller does not set is
    /// in its released state (that of [`Frame::clear_for_reuse`]).
    #[inline]
    pub(crate) fn push_builtin(
        &mut self,
        empty_code: &Rc<OpArray>,
        empty_vars: &Rc<RefCell<VarStore>>,
    ) -> &mut Frame {
        let idx = self.live;
        if idx < self.vec.len() {
            self.vec[idx].reinit_for_builtin(empty_code);
        } else {
            self.vec.push(Frame::empty_builtin(empty_code, empty_vars));
        }
        self.live = idx + 1;
        &mut self.vec[idx]
    }

    /// Makes sure a released frame sits right above the live ones: the
    /// frame a call made at this depth is readied in (`exec/tape/calls.rs`
    /// prepares its calls in place). The first call at a depth normally
    /// leaves one there; when none did (only calls that push no frame ran
    /// so far: frameless builtins), an empty one is appended, as
    /// [`FrameStack::push_builtin`] appends it.
    #[inline(always)]
    #[allow(clippy::inline_always)] // one length test per prepared call
    pub(crate) fn ensure_pooled(
        &mut self,
        empty_code: &Rc<OpArray>,
        empty_vars: &Rc<RefCell<VarStore>>,
    ) {
        if self.vec.len() <= self.live {
            self.grow_pool(empty_code, empty_vars);
        }
    }

    /// [`FrameStack::ensure_pooled`]'s append, out of line.
    #[cold]
    #[inline(never)]
    fn grow_pool(&mut self, empty_code: &Rc<OpArray>, empty_vars: &Rc<RefCell<VarStore>>) {
        self.vec.push(Frame::empty_builtin(empty_code, empty_vars));
    }

    /// Releases the builtin frame on top of the stack in place
    /// ([`Frame::clear_builtin_for_reuse`]), keeping it in the pool.
    #[inline]
    pub(crate) fn release_builtin_top(
        &mut self,
        empty_code: &Rc<OpArray>,
        empty_vars: &Rc<RefCell<VarStore>>,
    ) {
        if self.live > 0 {
            self.live -= 1;
            self.vec[self.live].clear_builtin_for_reuse(empty_code, empty_vars);
        }
    }

    /// Releases the topmost frame in place (retaining it in `vec[live..]` for pool reuse).
    #[inline]
    pub fn release_top(&mut self, empty_code: &Rc<OpArray>) {
        if self.live > 0 {
            let last_idx = self.live - 1;
            self.vec[last_idx].clear_for_reuse(empty_code);
            self.pop_cleared_top();
        }
    }

    /// [`FrameStack::release_top`] of a returning frame, with what
    /// [`crate::gc::frame_released`] records first, the calling frame's
    /// `$this` taken from the frame below ([`Frame::release_returning`]).
    #[inline]
    pub(crate) fn release_top_returning(&mut self, empty_code: &Rc<OpArray>) {
        if self.live > 0 {
            let (callers, rest) = self.vec.split_at_mut(self.live - 1);
            if let Some(frame) = rest.first_mut() {
                let caller_this = callers.last().and_then(|c| c.this.as_ref());
                frame.release_returning(caller_this, empty_code);
            }
            self.pop_cleared_top();
        }
    }

    /// The rest of [`FrameStack::release_top`] once the top frame has been
    /// released in place ([`Frame::clear_for_reuse`]) by a caller that
    /// already holds it (`Vm::return_simple`): the frame leaves the live
    /// ones and stays in the pool, which is cut back to 128 spare frames.
    ///
    /// Inline, with the cut out of line: as a call of its own it cost ~15
    /// instructions per return for a decrement and a comparison.
    #[inline(always)]
    #[allow(clippy::inline_always)] // once per return
    pub(crate) fn pop_cleared_top(&mut self) {
        if self.live > 0 {
            self.live -= 1;
            if self.vec.len() > self.live + 128 {
                self.trim_pool();
            }
        }
    }

    /// Cuts the pool of released frames back to 128 spare frames
    /// ([`FrameStack::pop_cleared_top`] after a deep recursion unwound).
    #[cold]
    #[inline(never)]
    fn trim_pool(&mut self) {
        self.vec.truncate(self.live + 128);
    }

    /// Frees the released frames kept in `vec[live..]` for reuse.
    ///
    /// php-src takes call frames from the VM stack, whose first page is
    /// allocated at startup, so calls never show in `memory_get_usage()`;
    /// the pooled frames' buffers do. Freeing the pool before reading the
    /// usage makes every reading see an empty pool, so that it does not
    /// depend on the calls made before (Symfony's
    /// NativeSessionStorageTest).
    ///
    /// The live builtin frames, which may sit in pooled frames that kept
    /// the buffers of an earlier user call ([`FrameStack::push_builtin`]),
    /// give those back too and share `empty_vars` again: they own what a
    /// builtin frame built afresh owns, and nothing more.
    pub fn drop_pool(&mut self, empty_vars: &Rc<RefCell<VarStore>>) {
        self.vec.truncate(self.live);
        for f in &mut self.vec {
            if f.kind == FrameKind::Builtin {
                f.shrink_builtin(empty_vars);
            }
        }
    }

    pub fn drain<R>(&mut self, range: R) -> std::vec::Drain<'_, Frame>
    where
        R: std::ops::RangeBounds<usize>,
    {
        let start = match range.start_bound() {
            std::ops::Bound::Included(&s) => s,
            std::ops::Bound::Excluded(&s) => s + 1,
            std::ops::Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            std::ops::Bound::Included(&e) => (e + 1).min(self.live),
            std::ops::Bound::Excluded(&e) => e.min(self.live),
            std::ops::Bound::Unbounded => self.live,
        };
        let count = end.saturating_sub(start);
        self.live = self.live.saturating_sub(count);
        self.vec.drain(start..end)
    }

    pub fn extend<I: IntoIterator<Item = Frame>>(&mut self, iter: I) {
        for f in iter {
            self.push(f);
        }
    }

    pub fn split_off(&mut self, at: usize) -> Vec<Frame> {
        if at >= self.live {
            return Vec::new();
        }
        let count = self.live - at;
        self.live = at;
        self.vec.drain(at..(at + count)).collect()
    }

    pub fn splice<R, I>(&mut self, range: R, replace_with: I) -> Vec<Frame>
    where
        R: std::ops::RangeBounds<usize>,
        I: IntoIterator<Item = Frame>,
    {
        self.vec.truncate(self.live);
        let removed: Vec<Frame> = self.vec.splice(range, replace_with).collect();
        self.live = self.vec.len();
        removed
    }

    pub fn insert(&mut self, index: usize, element: Frame) {
        self.vec.truncate(self.live);
        self.vec.insert(index, element);
        self.live += 1;
    }

    pub fn into_vec(mut self) -> Vec<Frame> {
        self.vec.truncate(self.live);
        self.vec
    }
}

impl IntoIterator for FrameStack {
    type Item = Frame;
    type IntoIter = std::vec::IntoIter<Frame>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl std::ops::Deref for FrameStack {
    type Target = [Frame];
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl std::ops::DerefMut for FrameStack {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<I: std::slice::SliceIndex<[Frame]>> std::ops::Index<I> for FrameStack {
    type Output = I::Output;
    #[inline(always)]
    fn index(&self, index: I) -> &Self::Output {
        &self.as_slice()[index]
    }
}

impl<I: std::slice::SliceIndex<[Frame]>> std::ops::IndexMut<I> for FrameStack {
    #[inline(always)]
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        &mut self.as_mut_slice()[index]
    }
}

impl From<Vec<Frame>> for FrameStack {
    fn from(v: Vec<Frame>) -> Self {
        let live = v.len();
        Self { vec: v, live }
    }
}

impl<'a> IntoIterator for &'a FrameStack {
    type Item = &'a Frame;
    type IntoIter = std::slice::Iter<'a, Frame>;
    #[inline(always)]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a mut FrameStack {
    type Item = &'a mut Frame;
    type IntoIter = std::slice::IterMut<'a, Frame>;
    #[inline(always)]
    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

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
    /// Compiled variables (inline values).
    pub cvs: Vec<CvSlot>,
    /// Whether a symbol table has been attached.
    pub symtab_attached: bool,
    /// The attached table's slot of each compiled variable, as
    /// [`Frame::attach_symbol_table`] found them, for the table at
    /// `symtab_slots_of` (a table's slots never move while it lives): an
    /// included file's variables are not where the global table has its
    /// names, and each synchronisation would look them up by name.
    pub(crate) symtab_slots: Vec<u32>,
    pub(crate) symtab_slots_of: *const RefCell<VarStore>,
    /// Variables / attached symbol table.
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
    /// Made current by the tape's call protocol (`exec::tape::calls`),
    /// which leaves every field but the slots, the passed arguments and the
    /// resume state as [`Frame::clear_for_reuse`] leaves them: released by
    /// [`Frame::release_returning`].
    pub(crate) direct: bool,
    /// The passed-argument count of a frame the tape's call protocol made
    /// current (`ZEND_CALL_NUM_ARGS`): its arguments are all in their
    /// compiled variables, so `args` stays empty instead of holding a
    /// `None` per argument. See [`Frame::num_args`].
    pub(crate) direct_argc: u32,
    /// Released by [`Frame::release_returning`] and untouched since: every
    /// field is in its released state, the slot vectors are sized for the
    /// code of `ufunc`, which the frame still holds (`keeps_function`),
    /// and the variable store is empty ([`Frame::is_released_for`]). A
    /// call of that same function readies it with nothing left to do
    /// (`exec::tape::calls::ready_frame`). Cleared by every other release
    /// and by every readying.
    pub(crate) released_direct: bool,
}

/// What the shared compiled-variable names of a frame's variable store
/// (`VarNames::shared`) are, as [`Frame::clear_for_reuse`] found them, so
/// that the rest of the release goes back to the store only to change
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SharedNames {
    /// Not looked at.
    Unknown,
    /// None.
    None,
    /// Those of the frame's code.
    Code,
    /// Those of another code.
    Other,
}

/// Keep small call buffers allocation-free without retaining exceptional frame sizes.
///
/// An empty buffer is left alone: `Vec::clear` of an empty vector still
/// calls the element type's out-of-line slice `drop_in_place` (measured on
/// every return for the named and positional argument buffers).
#[inline]
fn clear_frame_buffer<T>(buffer: &mut Vec<T>) {
    if buffer.capacity() > 256 {
        *buffer = Vec::new();
    } else if !buffer.is_empty() {
        buffer.clear();
    }
}

/// [`clear_frame_buffer`] of the passed-argument buffer of a recycled
/// frame, without the out-of-line slice drop glue of `Vec::clear`: a call
/// with arguments leaves one entry per argument, `None` once moved into its
/// compiled variable (`ZEND_CALL_NUM_ARGS`), so the buffer is usually
/// non-empty but owns nothing. The values left in it (extra arguments) are
/// released front to back, as `clear` releases them.
#[inline]
fn clear_arg_buffer(buffer: &mut Vec<Option<Value>>) {
    if buffer.capacity() > 256 {
        *buffer = Vec::new();
        return;
    }
    for arg in &mut *buffer {
        if let Some(v) = arg.take() {
            release_slot(v);
        }
    }
    // SAFETY: every element is `None` now, which owns nothing, so cutting
    // the length to zero forgets no value (the elements are initialised
    // and `0 <= capacity`).
    unsafe { buffer.set_len(0) };
}

/// [`clear_arg_buffer`] of a returning builtin's frame
/// ([`Frame::release_native`]): every entry holds a sent argument, which
/// the caller's variable usually still holds too, so each is released as
/// [`release_cv`] releases a compiled variable (a handle that is not the
/// last decremented inline, without `Value`'s out-of-line drop glue: ~30
/// instructions an argument), front to back.
#[inline(always)]
#[allow(clippy::inline_always)] // once per tape builtin call
fn clear_native_args(buffer: &mut Vec<Option<Value>>) {
    if buffer.capacity() > 256 {
        *buffer = Vec::new();
        return;
    }
    for arg in &mut *buffer {
        if let Some(v) = arg.take() {
            release_cv(v);
        }
    }
    // SAFETY: every element is `None` now, which owns nothing, so cutting
    // the length to zero forgets no value (the elements are initialised
    // and `0 <= capacity`).
    unsafe { buffer.set_len(0) };
}

/// Empties an `Option` field of a frame being recycled. Only a set field is
/// written: storing `None` over an unknown value calls the payload's
/// out-of-line `drop_in_place` (an object handle's) even when it was
/// already `None`, the common case for most fields of a returning frame.
#[inline(always)]
#[allow(clippy::inline_always)] // a dozen fields per return
fn clear_opt<T>(slot: &mut Option<T>) {
    if slot.is_some() {
        *slot = None;
    }
}

/// Empties an object-handle field (`$this`, the closure) of a frame
/// being recycled: a handle that is not the object's last is released
/// inline (`GC_DELREF`), without the call into `Option<Obj>`'s
/// out-of-line drop glue (~18 instructions a method or closure return);
/// the last one, whose release may run a destructor, is left to that glue
/// in place, as [`clear_opt`] leaves it (the field still holds the handle
/// while it is released).
#[inline(always)]
#[allow(clippy::inline_always)] // twice per direct return
fn release_obj_field(slot: &mut Option<Obj>) {
    let last = match slot.as_ref() {
        None => return,
        Some(o) => o.refcount() <= 1,
    };
    if last {
        release_last_obj_field(slot);
    } else if let Some(o) = slot.take() {
        drop(o);
    }
}

/// [`release_obj_field`] of an object's last handle, out of line.
#[cold]
#[inline(never)]
fn release_last_obj_field(slot: &mut Option<Obj>) {
    *slot = None;
}

/// Releases a value taken out of a compiled variable of a frame being
/// released (`i_free_compiled_variables`): like [`release_slot`], and a
/// string, or a handle of an object, array or reference that is not the
/// last, is released inline, its count decremented (`GC_DELREF`), without
/// the call into `Value`'s out-of-line drop glue and its dispatch on the
/// tag again. A last handle, which frees, is left to the drop glue.
#[inline(always)]
#[allow(clippy::inline_always)] // once per compiled variable of a return
fn release_cv(v: Value) {
    match v {
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => std::mem::forget(v),
        Value::Str(s) => drop(s),
        Value::Object(o) if o.refcount() > 1 => drop(o),
        Value::Array(a) if a.refcount() > 1 => drop(a),
        Value::Reference(r) if r.refcount() > 1 => drop(r),
        other => drop(other),
    }
}

/// Releases a value taken out of a recycled frame's slot: scalars own
/// nothing and are forgotten (no `drop_in_place` call); anything else is
/// dropped as before.
#[inline(always)]
#[allow(clippy::inline_always)] // once per temporary and compiled variable
fn release_slot(v: Value) {
    if matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    ) {
        std::mem::forget(v);
    } else {
        drop(v);
    }
}

/// Releases a compiled variable of a returning frame in place
/// (`i_free_compiled_variables`, with its `gc_check_possible_root`): the
/// tag is tested where the value is, so a scalar costs the store of the
/// undefined state and nothing else (no move of the value to the stack,
/// no dispatch on its tag again, as [`release_cv`] after `take` did), a
/// string an inline count decrement, and an array, an object or a
/// reference one call ([`release_rooted_cv`]).
#[inline(always)]
#[allow(clippy::inline_always)] // once per compiled variable of a return
fn release_cv_in_place(slot: &mut Option<Value>) {
    match slot {
        None => {}
        Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
            // Owns nothing: the value is forgotten, not dropped.
            std::mem::forget(slot.take());
        }
        Some(Value::Str(_)) => match slot.take() {
            Some(Value::Str(s)) => drop(s),
            // Not reached: the slot held a string just above.
            other => drop(other),
        },
        Some(_) => release_rooted_cv(slot),
    }
}

/// [`release_cv_in_place`] of an array, an object or a reference: what
/// `crate::gc::cvs_released` records for it ([`crate::gc::possible_root`]:
/// a handle released to a non-zero count, or the elements of an array
/// whose last handle goes), then its release. Out of line: the call is
/// that of `possible_root` it replaces.
#[inline(never)]
fn release_rooted_cv(slot: &mut Option<Value>) {
    if let Some(v) = slot.take() {
        crate::gc::possible_root(&v);
        release_cv(v);
    }
}

/// Empties `v` and fills it with `n` values made by `make` (the
/// temporaries, iterators, finally slots and compiled variables of one
/// function, in a pooled frame buffer that usually has the capacity
/// already).
#[inline]
pub(crate) fn refill_with<T>(v: &mut Vec<T>, n: usize, make: impl Fn() -> T) {
    // A recycled buffer is usually empty already (see `clear_frame_buffer`).
    if !v.is_empty() {
        v.clear();
    }
    if n != 0 {
        // `resize_with` stores the length once, after the loop; a `push`
        // per element stored it every time (~9 instructions an element).
        v.resize_with(n, make);
    }
}

/// Cuts or grows `v`, whose elements are all in their released state, to
/// `n` elements, making the new ones with `make`: the ones kept are not
/// written ([`Frame::reinit_for_call`]).
#[inline]
fn fit_released<T>(v: &mut Vec<T>, n: usize, make: impl Fn() -> T) {
    if v.len() > n {
        forget_released(v, n);
    } else if v.len() < n {
        v.resize_with(n, make);
    }
}

/// Empties `v`, whose elements are all in their released state
/// ([`fit_released`] to no element; [`Frame::reinit_for_builtin`]).
#[inline]
fn cut_released<T>(v: &mut Vec<T>) {
    forget_released(v, 0);
}

/// Cuts `v` to `n` elements without dropping the ones cut, which own
/// nothing (released slots, scalars): each is popped and forgotten, a
/// loop that reads nothing it uses and compiles to one store of the
/// length. `truncate` and `clear` run the drop glue of every element (a
/// jump on the tag of each temporary, or a call of the out-of-line glue:
/// ~10 instructions each), and a forgotten `drain` leaves a call of the
/// drain's own out-of-line drop (~22 instructions a cut).
#[inline(always)]
#[allow(clippy::inline_always)] // a store, in the callers' own loops
pub(crate) fn forget_released<T>(v: &mut Vec<T>, n: usize) {
    while v.len() > n {
        std::mem::forget(v.pop());
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
#[allow(dead_code)]
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
            cvs: Vec::with_capacity(8),
            symtab_attached: false,
            symtab_slots: Vec::new(),
            symtab_slots_of: std::ptr::null(),
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
            direct: false,
            direct_argc: 0,
            released_direct: false,
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
        clear_frame_buffer(&mut self.cvs);
        self.symtab_attached = false;
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
        self.direct = false;
        self.direct_argc = 0;
        self.released_direct = false;
    }

    /// Clears a frame for pool reuse while retaining its compiled code and buffer sizes.
    ///
    /// Every PHP object, closure, generator, value in CVs and temporaries is dropped
    /// so no observable state or handles survive, but the allocated vectors (`tmps`, `cvs`,
    /// `iters`, `fins`) retain their capacity and element counts so that calling the same
    /// function requires zero reallocation.
    ///
    /// The values are released in the same order as before; only fields
    /// that hold something are written ([`clear_opt`], [`release_slot`]),
    /// which keeps the `drop_in_place` calls of empty fields off the
    /// return path.
    ///
    /// The variable store is reached once (one `Rc::get_mut`): what its
    /// shared names are is noted here for `clear_rest_for_reuse`, which
    /// only goes back to the store when they must change. Nothing reads or
    /// writes the store in between: the handles released there queue
    /// their destructors (no user code runs), and the frame's code is
    /// replaced only after its names are dealt with.
    #[inline]
    pub fn clear_for_reuse(&mut self, empty_code: &Rc<OpArray>) {
        self.clear_slots_for_reuse();
        let shared = if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            clear_frame_buffer(&mut vs.slots);
            // The shared names are dropped in `clear_rest_for_reuse`, with
            // the function handles (`keeps_function`): they are the code's.
            clear_frame_buffer(&mut vs.names.owned);
            clear_opt(&mut vs.index);
            match &vs.names.shared {
                None => SharedNames::None,
                Some(c) if Rc::ptr_eq(c, &self.code) => SharedNames::Code,
                Some(_) => SharedNames::Other,
            }
        } else {
            self.vars = Rc::new(RefCell::new(VarStore::default()));
            SharedNames::None
        };
        self.clear_rest_for_reuse(empty_code, shared);
    }

    /// [`Frame::clear_for_reuse`] of a frame a prepared call readied and
    /// never entered (`exec::tape::calls::materialize`, the call turned
    /// back into a pending one, which took the arguments sent into its
    /// compiled variables and the `$this`, scopes and closure readied
    /// with it): such a frame holds nothing, and when it still holds the
    /// function as a released frame keeps it (`keeps_function`), it is in
    /// exactly the state [`Frame::release_returning`] leaves, so it is marked
    /// so ([`Frame::released_direct`]) and the next call of that function
    /// in this slot readies nothing. The fields readying and sending could
    /// have set are tested; anything else (a frame that was entered, a
    /// function not kept) takes the full release. The full release of
    /// such an empty frame walked every slot and field again (~100
    /// instructions for each call whose argument started another call:
    /// `f(g($x))`).
    #[inline]
    pub(crate) fn release_unentered(&mut self, empty_code: &Rc<OpArray>) {
        if self.kind == FrameKind::Function
            && !self.direct
            && self.ip == 0
            && !self.symtab_attached
            && self.this.is_none()
            && self.closure.is_none()
            && self.scope.is_none()
            && self.called.is_none()
            && self.args.is_empty()
            && self.named.is_empty()
            && self.calls.is_empty()
            && self.silence_saved.is_empty()
            && matches!(self.ret, RetTarget::Discard)
            && !self.is_ctor
            && self.keeps_function()
        {
            debug_assert!(self.slots_released());
            debug_assert!(self
                .ufunc
                .as_ref()
                .is_some_and(|uf| self.is_released_for(uf)));
            self.released_direct = true;
            return;
        }
        self.clear_for_reuse(empty_code);
    }

    /// [`Frame::clear_for_reuse`] of a builtin frame (the one
    /// [`FrameStack::push_builtin`] readied), whose variable store is
    /// either its own (kept from a pooled user frame, and emptied here) or
    /// the shared `empty_vars`, which is kept as it is: a frame left
    /// sharing it gets its own store from the next user call made in it
    /// ([`Frame::reinit_same_call`], [`Frame::reinit_for_call`]).
    #[inline]
    pub(crate) fn clear_builtin_for_reuse(
        &mut self,
        empty_code: &Rc<OpArray>,
        empty_vars: &Rc<RefCell<VarStore>>,
    ) {
        // Empty unless the handler used them: nothing to loop over.
        self.clear_slots_for_reuse();
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            clear_frame_buffer(&mut vs.slots);
            vs.names.clear();
            clear_opt(&mut vs.index);
        } else if !Rc::ptr_eq(&self.vars, empty_vars) {
            self.vars = empty_vars.clone();
        }
        self.clear_rest_for_reuse(empty_code, SharedNames::Unknown);
    }

    /// Readies a pooled frame (released by [`Frame::clear_for_reuse`] or
    /// [`Frame::clear_builtin_for_reuse`]) for a builtin call: the empty op
    /// array and no compiled variables, temporaries, iterators or finally
    /// slots (their buffers keep their capacity; the released slots own
    /// nothing). The variable store is kept: it is empty, either the
    /// frame's own or the shared `empty_vars`.
    ///
    /// Always inline: with the tape's builtin call as a second caller it
    /// came out of line, which cost every generic builtin call ~13
    /// instructions.
    #[inline(always)]
    #[allow(clippy::inline_always)]
    pub(crate) fn reinit_for_builtin(&mut self, empty_code: &Rc<OpArray>) {
        debug_assert!(self.slots_released());
        self.released_direct = false;
        self.kind = FrameKind::Builtin;
        // The slots of the user function called in this frame before are
        // released (they own nothing) and are cut as
        // [`Frame::reinit_for_call`] cuts them: `clear` ran the drop glue
        // of every temporary, out of line (490,000 times a request of a
        // framework application, where builtin and user calls alternate
        // at every depth).
        cut_released(&mut self.tmps);
        cut_released(&mut self.iters);
        cut_released(&mut self.fins);
        cut_released(&mut self.cvs);
        // Kept by `keeps_function` for a user function.
        clear_opt(&mut self.ufunc);
        if !Rc::ptr_eq(&self.code, empty_code) {
            self.code = empty_code.clone();
        }
    }

    /// The frame [`FrameStack::push_builtin`] appends when no pooled frame
    /// is left: the frame `Frame::with_vars` makes for the empty op array,
    /// built without allocating.
    fn empty_builtin(empty_code: &Rc<OpArray>, empty_vars: &Rc<RefCell<VarStore>>) -> Self {
        Frame {
            tmps: Vec::new(),
            iters: Vec::new(),
            fins: Vec::new(),
            calls: Vec::new(),
            cvs: Vec::new(),
            symtab_attached: false,
            symtab_slots: Vec::new(),
            symtab_slots_of: std::ptr::null(),
            vars: empty_vars.clone(),
            named: Vec::new(),
            args: Vec::new(),
            this: None,
            closure: None,
            generator: None,
            pending_exc: None,
            statics: None,
            func: None,
            ufunc: None,
            code: empty_code.clone(),
            ip: 0,
            scope: None,
            called: None,
            ret: RetTarget::Discard,
            kind: FrameKind::Builtin,
            is_ctor: false,
            silence_saved: Vec::new(),
            builtin_name: None,
            magic_name: None,
            caller_strict: false,
            called_from_internal: false,
            observed: false,
            observer_hidden: false,
            direct: false,
            direct_argc: 0,
            released_direct: false,
        }
    }

    /// Gives back the buffers a live builtin frame kept from the pooled
    /// frame it was readied in, and makes it share `empty_vars` again
    /// ([`FrameStack::drop_pool`]).
    fn shrink_builtin(&mut self, empty_vars: &Rc<RefCell<VarStore>>) {
        fn shrink<T>(v: &mut Vec<T>) {
            if v.is_empty() && v.capacity() > 0 {
                *v = Vec::new();
            }
        }
        // A pooled frame keeps its cleared slots at their length for the
        // next call of the same function; a builtin frame uses none, and
        // the call paths resize them when the lengths do not match.
        if self.tmps.iter().all(|v| matches!(v, Value::Null)) {
            self.tmps = Vec::new();
        }
        shrink(&mut self.iters);
        shrink(&mut self.fins);
        shrink(&mut self.calls);
        if self.cvs.iter().all(CvSlot::is_none) {
            self.cvs = Vec::new();
        }
        shrink(&mut self.named);
        shrink(&mut self.silence_saved);
        // The passed-argument buffer an earlier user call left behind.
        if self.args.iter().all(Option::is_none) {
            self.args = Vec::new();
        }
        // Only a store of its own that holds nothing (one kept from a
        // pooled frame, its slots cleared) is given back.
        let own_and_empty = Rc::get_mut(&mut self.vars)
            .is_some_and(|rc_mut| rc_mut.get_mut().slots.iter().all(Option::is_none));
        if own_and_empty {
            self.vars = empty_vars.clone();
        }
    }

    /// The compiled variables, temporaries, iterators, finally states and
    /// pending calls of [`Frame::clear_for_reuse`].
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every return
    fn clear_slots_for_reuse(&mut self) {
        for t in &mut self.tmps {
            release_slot(std::mem::take(t));
        }
        // Foreach iterators, finally states and pending calls: absent from
        // most functions (`num_iters`/`num_finally` are zero) and empty
        // after a return.
        if !(self.iters.is_empty() && self.fins.is_empty() && self.calls.is_empty())
            || self.calls.capacity() > 256
        {
            self.clear_loop_state();
        }
        for slot in &mut self.cvs {
            if let Some(old) = slot.as_mut().take() {
                release_cv(old);
            }
        }
        self.symtab_attached = false;
    }

    /// The fields after the variable store of [`Frame::clear_for_reuse`].
    /// `shared` is what the store's shared names are, when the caller
    /// looked ([`SharedNames`]).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every return
    fn clear_rest_for_reuse(&mut self, empty_code: &Rc<OpArray>, shared: SharedNames) {
        if !self.named.is_empty() || self.named.capacity() > 256 {
            clear_frame_buffer(&mut self.named);
        }
        clear_arg_buffer(&mut self.args);
        clear_opt(&mut self.this);
        clear_opt(&mut self.closure);
        // Generator, caught exception and static variables: one test for
        // the three fields a plain call leaves empty.
        if self.generator.is_some() || self.pending_exc.is_some() || self.statics.is_some() {
            self.clear_rare_handles();
        }
        if self.keeps_function() {
            // The variable store's names stay shared with the kept code,
            // which the next call of the same function shares again
            // (`VarNames::share`); a different code replaces them
            // (`reinit_same_call`, `reinit_for_call`). Known to be none or
            // the code's already (the common case): the store is left alone.
            if !matches!(shared, SharedNames::None | SharedNames::Code) {
                if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
                    let names = &mut rc_mut.get_mut().names;
                    if names
                        .shared
                        .as_ref()
                        .is_some_and(|c| !Rc::ptr_eq(c, &self.code))
                    {
                        names.shared = None;
                    }
                }
            }
        } else {
            if shared != SharedNames::None {
                if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
                    clear_opt(&mut rc_mut.get_mut().names.shared);
                }
            }
            clear_opt(&mut self.func);
            clear_opt(&mut self.ufunc);
            if !Rc::ptr_eq(&self.code, empty_code) {
                self.code = empty_code.clone();
            }
        }
        self.ip = 0;
        clear_opt(&mut self.scope);
        clear_opt(&mut self.called);
        self.ret = RetTarget::Discard;
        self.is_ctor = false;
        // `@` masks, builtin and `__call` names: one test for the three.
        if !self.silence_saved.is_empty()
            || self.builtin_name.is_some()
            || self.magic_name.is_some()
            || self.silence_saved.capacity() > 256
        {
            self.clear_rare_names();
        }
        self.caller_strict = false;
        self.called_from_internal = false;
        self.observed = false;
        self.observer_hidden = false;
        self.direct = false;
        self.direct_argc = 0;
        self.released_direct = false;
    }

    /// [`Frame::clear_for_reuse`] of a frame the tape's call protocol made
    /// current ([`Frame::direct`]): every other field is as that release
    /// leaves it, and the function's code can only have changed a few of
    /// them since, which are tested; then the slots and the passed
    /// arguments are released (`zend_leave_helper`'s one test of
    /// `call_info`, then its loop over the compiled variables). `false`,
    /// having released nothing, for any other frame, or when the code left
    /// more behind (a symbol table attached or used, a caught exception
    /// held, an `@` still open, a pending call, a function nothing else
    /// holds): the full `clear_for_reuse` then runs.
    ///
    /// For the tests of the released mark: a return goes through
    /// [`Frame::release_returning`], which also records the possible roots.
    #[cfg(test)]
    pub(crate) fn release_direct(&mut self) -> bool {
        if !self.direct_releasable() {
            return false;
        }
        // In `clear_for_reuse`'s order: the temporaries, the compiled
        // variables, the passed arguments.
        self.release_direct_tmps();
        for slot in &mut self.cvs {
            if let Some(old) = slot.as_mut().take() {
                release_cv(old);
            }
        }
        self.release_direct_args();
        self.release_direct_rest();
        true
    }

    /// The tests of [`Frame::release_returning`]: whether the frame is a
    /// directly entered one whose code left nothing behind but its slots,
    /// passed arguments and call handles. Changes nothing.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every direct return
    fn direct_releasable(&mut self) -> bool {
        if !self.direct
            || self.symtab_attached
            || self.pending_exc.is_some()
            || !self.silence_saved.is_empty()
            || !self.calls.is_empty()
            || !self.keeps_function()
        {
            return false;
        }
        let Some(rc_mut) = Rc::get_mut(&mut self.vars) else {
            return false;
        };
        let vs = rc_mut.get_mut();
        vs.names.owned.is_empty() && vs.index.is_none() && !vs.slots.iter().any(Option::is_some)
    }

    /// [`Frame::release_returning`]'s release of the temporaries.
    ///
    /// The tag is tested where the value is, as for the compiled
    /// variables ([`release_cv_in_place`]): most temporaries of a
    /// returning frame are `null` already (the op that read one took it),
    /// which costs the test alone, a scalar costs the store of `null`,
    /// and only a counted value is moved out and dropped. Taking every
    /// slot first moved each value to the stack and stored `null` before
    /// its tag was looked at (~11 instructions a temporary, a third of a
    /// return's instructions in a framework application's request).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every direct return
    fn release_direct_tmps(&mut self) {
        for t in &mut self.tmps {
            match t {
                Value::Null => {}
                Value::Bool(_) | Value::Int(_) | Value::Float(_) => {
                    std::mem::forget(std::mem::replace(t, Value::Null));
                }
                _ => drop(std::mem::take(t)),
            }
        }
    }

    /// [`Frame::release_returning`]'s release of the passed arguments.
    ///
    /// A direct frame's passed arguments are in its compiled variables
    /// and the buffer is empty (`calls::enter` keeps the count in
    /// `direct_argc`), except for a variadic call's extra arguments
    /// (`calls::variadic_extras`): it is emptied here, as
    /// `clear_arg_buffer` would, without the call (the drop of any value
    /// it held runs as there). Tested first: `clear` of the empty buffer
    /// still called the out-of-line slice drop glue (~12 instructions a
    /// return).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every direct return
    fn release_direct_args(&mut self) {
        if self.args.capacity() > 256 {
            self.args = Vec::new();
        } else if !self.args.is_empty() {
            self.args.clear();
        }
    }

    /// The rest of [`Frame::release_returning`] once the slots and the passed
    /// arguments are released: a method's `$this` and scopes, a closure,
    /// in `clear_rest_for_reuse`'s order, and the call's fields.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every direct return
    fn release_direct_rest(&mut self) {
        release_obj_field(&mut self.this);
        release_obj_field(&mut self.closure);
        self.ip = 0;
        clear_opt(&mut self.scope);
        clear_opt(&mut self.called);
        self.ret = RetTarget::Discard;
        self.is_ctor = false;
        self.caller_strict = false;
        self.direct = false;
        self.direct_argc = 0;
        self.called_from_internal = false;
        // Every field is in its released state now, and the function's
        // handles and slot sizes are kept: the next call of the same
        // function in this slot readies nothing (`calls::ready_frame`).
        self.released_direct = true;
    }

    /// `zend_leave_helper` of a returning user frame in one step: what
    /// [`crate::gc::frame_released`] records, then the release
    /// ([`Frame::release_returning`], else [`Frame::clear_for_reuse`]).
    /// `caller_this` is the calling frame's `$this`.
    ///
    /// A directly entered frame ([`Frame::direct_releasable`]) is released
    /// in one pass over its compiled variables, as
    /// `i_free_compiled_variables` releases them: a scalar is a tag test
    /// and a store, a string an inline count decrement, and an array, an
    /// object or a reference calls out once ([`release_rooted_cv`]: its
    /// possible root, then its release), in variable order; then the
    /// passed arguments, then the call's own handle (`$this`, a closure:
    /// [`crate::gc::call_handle_released`], then its release), as
    /// `zend_leave_helper` releases them after the variables. The roots
    /// are buffered in php-src's order: variable order, then the call's
    /// handle (a shared reference's root, which its release buffers, came
    /// after the call's handle when every other root was buffered by a
    /// walk before the release). That walk was a second pass over the
    /// variables (~8 instructions a variable), and the
    /// release moved each value to the stack and dispatched on its tag
    /// again (~15). Destructors are queued by the release (no user code
    /// runs in it), so nothing can observe the roots of the later
    /// variables not being buffered yet.
    ///
    /// Any other frame takes the two steps: the roots of every variable
    /// and of the call's handle (and of an attached symbol table's
    /// values), then the release.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of every return
    pub(crate) fn release_returning(
        &mut self,
        caller_this: Option<&Obj>,
        empty_code: &Rc<OpArray>,
    ) {
        if self.direct_releasable() {
            self.release_direct_tmps();
            for slot in &mut self.cvs {
                release_cv_in_place(slot.as_mut());
            }
            self.release_direct_args();
            crate::gc::call_handle_released(self, caller_this);
            self.release_direct_rest();
            return;
        }
        if self.symtab_attached {
            crate::gc::frame_released(self, caller_this);
        } else {
            crate::gc::cvs_released(&self.cvs);
            crate::gc::call_handle_released(self, caller_this);
        }
        self.clear_for_reuse(empty_code);
    }

    /// Whether the frame's variable store is its own and holds nothing:
    /// the store the last user call made in this pooled frame left (a
    /// released frame keeps its store for the next one,
    /// [`Frame::clear_for_reuse`]). A builtin's frame does not use it, and
    /// there is nothing in it to release.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `release_native`
    fn holds_empty_store(&mut self) -> bool {
        Rc::get_mut(&mut self.vars).is_some_and(|rc| {
            let vs = rc.get_mut();
            vs.slots.is_empty()
                && vs.names.shared.is_none()
                && vs.names.owned.is_empty()
                && vs.index.is_none()
        })
    }

    /// [`Frame::clear_builtin_for_reuse`] of the builtin frame the tape's
    /// internal call made current (`exec/tape/calls.rs`, `call_native`):
    /// that call set only its function, name and passed arguments, which
    /// are released (`zend_vm_stack_free_args`); every other field is still
    /// in the released state [`Frame::reinit_for_builtin`] left, which is
    /// tested. `false`, having released nothing, when the handler left
    /// more behind (a pending call, a symbol table, an `@`, named
    /// arguments, a variable store that holds something): the full release
    /// then runs.
    ///
    /// The store is the shared empty one in a frame only builtins used,
    /// and the frame's own in one a user function was called in before
    /// ([`Frame::holds_empty_store`]): in an application nearly every
    /// frame is of the second kind, and refusing those sent every builtin
    /// call of a request through the full release (62,700 of 64,400).
    ///
    /// Always inline (its two callers, the tape's call and
    /// `Vm::call_builtin_pooled`, run it once a call): out of line its
    /// call, register saves and restores cost ~17 instructions a call.
    #[inline(always)]
    #[allow(clippy::inline_always)]
    pub(crate) fn release_native(&mut self, empty_vars: &Rc<RefCell<VarStore>>) -> bool {
        if self.kind != FrameKind::Builtin
            || self.symtab_attached
            || !self.calls.is_empty()
            || !self.named.is_empty()
            || !self.silence_saved.is_empty()
            || self.magic_name.is_some()
            || self.this.is_some()
            || self.closure.is_some()
            || self.scope.is_some()
            || self.called.is_some()
            || self.generator.is_some()
            || self.pending_exc.is_some()
            || self.statics.is_some()
            || !self.tmps.is_empty()
            || !self.cvs.is_empty()
            || !(Rc::ptr_eq(&self.vars, empty_vars) || self.holds_empty_store())
        {
            return false;
        }
        clear_native_args(&mut self.args);
        // The builtin's handle released in line (the function table holds
        // the function too), where `Option<Func>`'s drop glue is an
        // out-of-line call dispatching on the kind again.
        match self.func.take() {
            Some(Func::Builtin(b)) => drop(b),
            None => {}
            other => drop(other),
        }
        clear_opt(&mut self.builtin_name);
        self.ip = 0;
        self.ret = RetTarget::Discard;
        self.is_ctor = false;
        self.caller_strict = false;
        self.called_from_internal = false;
        self.observed = false;
        self.observer_hidden = false;
        self.direct = false;
        self.direct_argc = 0;
        self.released_direct = false;
        true
    }

    /// The foreach iterators, finally states and pending calls of
    /// [`Frame::clear_for_reuse`], in that order.
    #[cold]
    #[inline(never)]
    fn clear_loop_state(&mut self) {
        for it in &mut self.iters {
            clear_opt(it);
        }
        for fin in &mut self.fins {
            if !matches!(fin, FinState::None) {
                *fin = FinState::None;
            }
        }
        clear_frame_buffer(&mut self.calls);
    }

    /// The generator, caught exception and static variables of
    /// [`Frame::clear_for_reuse`], in that order.
    #[cold]
    #[inline(never)]
    fn clear_rare_handles(&mut self) {
        clear_opt(&mut self.generator);
        clear_opt(&mut self.pending_exc);
        clear_opt(&mut self.statics);
    }

    /// The `@` masks, builtin name and `__call` name of
    /// [`Frame::clear_for_reuse`], in that order.
    #[cold]
    #[inline(never)]
    fn clear_rare_names(&mut self) {
        clear_frame_buffer(&mut self.silence_saved);
        clear_opt(&mut self.builtin_name);
        clear_opt(&mut self.magic_name);
    }

    /// The number of arguments passed to the call (`ZEND_CALL_NUM_ARGS`):
    /// the argument buffer's length, or the count a directly entered frame
    /// keeps instead ([`Frame::direct_argc`]).
    #[inline]
    pub(crate) fn num_args(&self) -> usize {
        self.args.len().max(self.direct_argc as usize)
    }

    /// Whether the frame is in the state [`Frame::release_returning`] leaves
    /// for a later call of `uf` ([`Frame::released_direct`]): what
    /// `calls::ready_frame` would otherwise set up for that call
    /// (`reinit_same_call` of its code, the function handles) is already
    /// so. For the debug assertion of that path.
    pub(crate) fn is_released_for(&self, uf: &Rc<UserFunction>) -> bool {
        let code = &uf.code;
        let store_empty = self.vars.try_borrow().is_ok_and(|vs| {
            vs.slots.iter().all(Option::is_none) && vs.index.is_none() && vs.names.owned.is_empty()
        });
        self.kind == FrameKind::Function
            && self.ip == 0
            && !self.direct
            && self.direct_argc == 0
            && !self.symtab_attached
            && self.calls.is_empty()
            && self.silence_saved.is_empty()
            && self.args.is_empty()
            && self.named.is_empty()
            && self.this.is_none()
            && self.closure.is_none()
            && self.scope.is_none()
            && self.called.is_none()
            && self.generator.is_none()
            && self.pending_exc.is_none()
            && self.statics.is_none()
            && !self.is_ctor
            && matches!(self.ret, RetTarget::Discard)
            && Rc::ptr_eq(&self.code, code)
            && self.ufunc.as_ref().is_some_and(|f| Rc::ptr_eq(f, uf))
            && matches!(&self.func, Some(Func::User(f)) if Rc::ptr_eq(f, uf))
            && self.cvs.len() == code.cv_names.len()
            && self.cvs.iter().all(CvSlot::is_none)
            && self.tmps.len() == code.num_tmps as usize
            && self.iters.len() == code.num_iters as usize
            && self.fins.len() == code.num_finally as usize
            && store_empty
    }

    /// Whether a frame being recycled keeps its function handles (`func`,
    /// `ufunc` and `code`), so that the next call of the same function in
    /// this slot (a loop calling it) does not replace them: when they are
    /// one user function and its code, the function has no static
    /// variables (`statics` stays empty: the pool holds no PHP value
    /// through it) and something besides this frame holds the function
    /// (dropping the frame's handles would free nothing). The handles are
    /// overwritten by the next call made in the slot.
    ///
    /// Inline: out of line it cost every direct return a call (16
    /// instructions and the call's own).
    #[inline(always)]
    #[allow(clippy::inline_always)]
    fn keeps_function(&self) -> bool {
        match (&self.ufunc, &self.func) {
            (Some(u), Some(Func::User(f))) => {
                Rc::ptr_eq(u, f)
                    && Rc::ptr_eq(&u.code, &self.code)
                    && u.code.num_statics == 0
                    && Rc::strong_count(u) > 2
            }
            _ => false,
        }
    }

    /// Re-initializes an existing frame when `cvs` and `tmps` already have the right lengths.
    #[inline(always)]
    pub(crate) fn reinit_same_call(&mut self, code: &Rc<OpArray>, kind: FrameKind) {
        self.released_direct = false;
        self.kind = kind;
        self.ip = 0;
        // Empty after a return: `clear` would still call the out-of-line
        // slice drop glue of `PendingCall`.
        if !self.calls.is_empty() {
            self.calls.clear();
        }
        self.silence_saved.clear();
        self.symtab_attached = false;
        // A recycled frame may still hold this code (`keeps_function`).
        let same_code = Rc::ptr_eq(&self.code, code);
        if !same_code {
            self.code = code.clone();
        }
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            if same_code {
                for slot in vs.slots.iter_mut() {
                    *slot = None;
                }
            } else {
                // Names kept shared with the previous code
                // (`clear_for_reuse`) are not this code's:
                // `attach_symbol_table` shares the new ones, and sizes the
                // slots for them (a store's slots run parallel to its
                // names: the previous code may have had more variables).
                if !vs.slots.is_empty() {
                    vs.slots.clear();
                }
                clear_opt(&mut vs.names.shared);
            }
        } else {
            // A pooled frame last used by a builtin may still share the
            // VM's `empty_vars` (`Frame::clear_builtin_for_reuse`): a
            // function without compiled variables passes the size checks
            // and must not write into that store.
            self.vars = Rc::new(RefCell::new(VarStore::default()));
        }
    }

    /// Re-initializes a pooled frame for a new user function call, leaving
    /// the compiled variables empty for the caller to fill (the arguments
    /// move into them, [`crate::call`]'s `init_cvs`).
    ///
    /// A pooled frame is released ([`Frame::slots_released`]: every
    /// temporary holds `null`, every compiled variable, iterator and
    /// finally state nothing), so only the lengths change: each slot vector
    /// is cut or grown to the code's count, and the slots it keeps are not
    /// written again (php-src's `i_init_func_execute_data` sets the
    /// variables after the arguments to undefined and leaves the
    /// temporaries alone). Emptying and refilling them cost ~250
    /// instructions a call of another function than the one the frame
    /// held (`Vec::clear` calls `Value`'s out-of-line drop glue for every
    /// temporary). The rest is [`Frame::reinit_same_call`]'s, which leaves
    /// the variable names to the first attach of a symbol table instead of
    /// sharing them on every call (`VarNames::share`).
    pub(crate) fn reinit_for_call(&mut self, code: &Rc<OpArray>, kind: FrameKind) {
        debug_assert!(self.slots_released());
        fit_released(&mut self.tmps, code.num_tmps as usize, || Value::Null);
        fit_released(&mut self.iters, code.num_iters as usize, || None);
        fit_released(&mut self.fins, code.num_finally as usize, || FinState::None);
        fit_released(&mut self.cvs, code.cv_names.len(), || CvSlot::new(None));
        self.reinit_same_call(code, kind);
    }

    /// Whether every slot of the frame is in its released state: the
    /// temporaries `null`, the compiled variables undefined, the foreach
    /// iterators and finally states empty. Every release leaves a frame
    /// so ([`Frame::release_returning`], [`Frame::clear_for_reuse`],
    /// [`Frame::clear_builtin_for_reuse`], [`Frame::clear_for_pool`] and a
    /// newly built frame); [`Frame::reinit_for_call`] relies on it. For
    /// debug assertions.
    pub(crate) fn slots_released(&self) -> bool {
        self.tmps.iter().all(Value::is_null)
            && self.cvs.iter().all(CvSlot::is_none)
            && self.iters.iter().all(Option::is_none)
            && self.fins.iter().all(|f| matches!(f, FinState::None))
    }

    /// Re-initializes a pooled frame for a new user function call, with
    /// every compiled variable undefined.
    pub fn reinit_for_user(&mut self, code: Rc<OpArray>, kind: FrameKind) {
        self.reinit_for_call(&code, kind);
        let ncv = self.code.cv_names.len();
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            vs.slots.clear();
            vs.names.share(&code);
            refill_with(&mut vs.slots, ncv, || None);
        } else {
            let mut vs = VarStore::for_code(&code);
            refill_with(&mut vs.slots, ncv, || None);
            self.vars = Rc::new(RefCell::new(vs));
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
        _cv_map: Option<Rc<[u32]>>,
    ) -> Self {
        let ncv = code.cv_names.len();
        let symtab_attached = matches!(kind, FrameKind::Include | FrameKind::Eval);
        // The main script's compiled variables get their symbol-table
        // entries now, as `zend_attach_symbol_table` adds them to
        // `EG(symbol_table)` when the script starts (php-src's
        // `i_init_code_execute_data`): adding them on the first generic
        // access instead would grow the global table mid-script, which
        // `memory_get_usage()` sees (ext/openssl/tests/bug79881.phpt) and
        // which would order them after variables created before that.
        let add_names = symtab_attached || matches!(kind, FrameKind::Main);
        let mut cvs = Vec::with_capacity(ncv);
        // An attached frame's slots, as `attach_symbol_table` records them.
        let mut symtab_slots = Vec::new();
        let symtab_slots_of = if symtab_attached {
            Rc::as_ptr(&vars)
        } else {
            std::ptr::null()
        };
        {
            let mut st = vars.borrow_mut();
            for name in &code.cv_names {
                let val = if add_names {
                    let slot = st.find_or_add(name.as_bytes());
                    // The main script's slots too, though its table is
                    // attached later (`attach_symbol_table` reuses the
                    // list's room): recording them on the first generic
                    // access (an `unset()` at the top level) would
                    // allocate mid-script (bug79881).
                    record_slot(&mut symtab_slots, cvs.len(), slot, ncv);
                    let slot = slot as usize;
                    st.slots.get(slot).cloned().flatten()
                } else {
                    st.find(name.as_bytes())
                        .and_then(|slot| st.slots.get(slot as usize).cloned().flatten())
                };
                cvs.push(CvSlot::new(val));
            }
        }
        let ntmps = code.num_tmps as usize;
        let niters = code.num_iters as usize;
        let nfin = code.num_finally as usize;
        Frame {
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
            calls: Vec::with_capacity(4),
            cvs,
            symtab_attached,
            symtab_slots,
            symtab_slots_of,
            vars,
            named: Vec::new(),
            args: Vec::with_capacity(8),
            this: None,
            closure: None,
            generator: None,
            pending_exc: None,
            statics: None,
            func: None,
            ufunc: None,
            code,
            ip: 0,
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
            direct: false,
            direct_argc: 0,
            released_direct: false,
        }
    }

    /// Attaches the symbol table to this frame on demand (`zend_attach_symbol_table`),
    /// populating it from the current CV slots.
    pub fn attach_symbol_table(&mut self) {
        if self.symtab_attached {
            return;
        }
        self.symtab_attached = true;
        let mut st = self.vars.borrow_mut();
        if st.names.is_empty() {
            st.names.share(&self.code);
            if st.slots.len() < self.code.cv_names.len() {
                st.slots.resize(self.code.cv_names.len(), None);
            }
        }
        self.symtab_slots.clear();
        self.symtab_slots_of = Rc::as_ptr(&self.vars);
        for (i, name) in self.code.cv_names.iter().enumerate() {
            let slot = st.find_or_add_cv(i, name.as_bytes());
            record_slot(&mut self.symtab_slots, i, slot, self.code.cv_names.len());
            let slot = slot as usize;
            if slot < st.slots.len() {
                if let Some(val) = self.cvs.get(i).and_then(CvSlot::get) {
                    st.slots[slot] = Some(val.clone());
                }
            }
        }
    }

    /// Undoes an [`Frame::attach_symbol_table`] that only a generic
    /// variable access needed (the place walk of `place.rs` works on the
    /// table's slots): the table's value of each compiled variable
    /// moves back into the CV slot (the table is authoritative while
    /// attached, as in [`Frame::sync_from_symbol_table`]) and the table
    /// keeps no duplicate handle, so later in-place writes see refcount 1
    /// and no write has to be mirrored. Variables the table holds that are
    /// not compiled variables stay in it. The next attach copies the CVs
    /// back in.
    pub fn detach_symbol_table(&mut self) {
        if !self.symtab_attached {
            return;
        }
        self.symtab_attached = false;
        let valid = std::ptr::eq(self.symtab_slots_of, Rc::as_ptr(&self.vars));
        let mut st = self.vars.borrow_mut();
        for (i, name) in self.code.cv_names.iter().enumerate() {
            let Some(idx) = mapped_slot(&self.symtab_slots, valid, &mut st, i, name) else {
                continue;
            };
            let Some(sv) = st.slots.get_mut(idx as usize) else {
                continue;
            };
            let val = sv.take();
            if let Some(cv) = self.cvs.get_mut(i) {
                if let Some(old) = cv.set(val) {
                    crate::gc::release_duplicate(old);
                }
            }
        }
    }

    /// Synchronizes CV slots from the attached symbol table (`zend_detach_symbol_table`).
    /// A CV already holding the slot's value (the same handle) is left as
    /// it is: most are, when the table is synchronised after each write.
    pub fn sync_from_symbol_table(&mut self) {
        if !self.symtab_attached {
            return;
        }
        let valid = std::ptr::eq(self.symtab_slots_of, Rc::as_ptr(&self.vars));
        let mut st = self.vars.borrow_mut();
        for (i, name) in self.code.cv_names.iter().enumerate() {
            if let Some(slot) = mapped_slot(&self.symtab_slots, valid, &mut st, i, name) {
                let Some(cv) = self.cvs.get_mut(i) else {
                    continue;
                };
                let table = st.slots.get(slot as usize).and_then(Option::as_ref);
                match (cv.get(), table) {
                    (None, None) => continue,
                    (Some(a), Some(b)) if same_handle(a, b) => continue,
                    _ => {}
                }
                if let Some(old) = cv.set(table.cloned()) {
                    crate::gc::release_duplicate(old);
                }
            }
        }
    }

    /// Synchronizes all CV slots into the attached symbol table.
    pub fn sync_cvs_to_symtab(&self) {
        if !self.symtab_attached {
            return;
        }
        let valid = std::ptr::eq(self.symtab_slots_of, Rc::as_ptr(&self.vars));
        let mut st = self.vars.borrow_mut();
        for (i, name) in self.code.cv_names.iter().enumerate() {
            if let Some(slot) = mapped_slot(&self.symtab_slots, valid, &mut st, i, name) {
                let slot = slot as usize;
                if slot < st.slots.len() {
                    // A CV that is `None` was never assigned in this frame
                    // (e.g. cleared by `clear_cv_for_symtab_slot` ahead of an
                    // in-place mutation of the shared slot); it must not wipe
                    // a live global slot that another frame still owns.
                    let Some(cv) = self.cvs.get(i).and_then(CvSlot::get) else {
                        continue;
                    };
                    if st.slots[slot].as_ref().is_some_and(|t| same_handle(t, cv)) {
                        continue;
                    }
                    if let Some(old) = std::mem::replace(&mut st.slots[slot], Some(cv.clone())) {
                        crate::gc::release_duplicate(old);
                    }
                }
            }
        }
    }

    /// Drops the duplicate handle held by the attached symbol table for `cv`
    /// before an in-place mutation of `self.cvs[cv]`, so a value owned only by
    /// this variable sees refcount 1 (`is_unique_mutable` / `make_mut`).
    /// Call [`Frame::sync_cv_to_symtab`] once the mutation is complete.
    #[inline(always)]
    pub fn unsync_cv_from_symtab(&self, cv: u32) {
        if self.symtab_attached {
            self.unsync_cv_from_symtab_slow(cv);
        }
    }

    #[cold]
    #[inline(never)]
    fn unsync_cv_from_symtab_slow(&self, cv: u32) {
        if let Some(name) = self.code.cv_names.get(cv as usize) {
            let valid = std::ptr::eq(self.symtab_slots_of, Rc::as_ptr(&self.vars));
            let mut st = self.vars.borrow_mut();
            let idx = mapped_slot(&self.symtab_slots, valid, &mut st, cv as usize, name);
            if let Some(idx) = idx {
                let idx = idx as usize;
                if idx < st.slots.len() {
                    if let Some(old) = std::mem::replace(&mut st.slots[idx], None) {
                        crate::gc::release_duplicate(old);
                    }
                }
            }
        }
    }

    /// Clears the CV slot mirroring `sym_idx` of `store` before an in-place
    /// mutation of `store.slots[sym_idx]` that will be followed by
    /// [`Frame::sync_from_symbol_table`].
    #[inline]
    pub fn clear_cv_for_symtab_slot(&mut self, store: &Rc<RefCell<VarStore>>, sym_idx: usize) {
        if self.symtab_attached && Rc::ptr_eq(store, &self.vars) {
            let st = self.vars.borrow();
            if let Some(name) = st.names.get(sym_idx) {
                if self
                    .code
                    .cv_names
                    .get(sym_idx)
                    .is_some_and(|cv_name| cv_name.as_bytes() == name.as_bytes())
                {
                    if let Some(slot) = self.cvs.get_mut(sym_idx) {
                        if let Some(old) = slot.set(None) {
                            crate::gc::release_duplicate(old);
                        }
                    }
                } else if let Some(cv_idx) = self
                    .code
                    .cv_names
                    .iter()
                    .position(|cv_name| cv_name.as_bytes() == name.as_bytes())
                {
                    if let Some(slot) = self.cvs.get_mut(cv_idx) {
                        if let Some(old) = slot.set(None) {
                            crate::gc::release_duplicate(old);
                        }
                    }
                }
            }
        }
    }

    /// Synchronizes a single modified CV slot into the attached symbol table.
    #[inline(always)]
    pub fn sync_cv_to_symtab(&self, cv: u32, v: &Value) {
        if self.symtab_attached {
            self.sync_cv_to_symtab_slow(cv, v);
        }
    }

    #[cold]
    #[inline(never)]
    fn sync_cv_to_symtab_slow(&self, cv: u32, v: &Value) {
        if let Some(name) = self.code.cv_names.get(cv as usize) {
            let valid = std::ptr::eq(self.symtab_slots_of, Rc::as_ptr(&self.vars));
            let mut st = self.vars.borrow_mut();
            let idx = match mapped_slot(&self.symtab_slots, valid, &mut st, cv as usize, name) {
                Some(i) => i,
                None => st.find_or_add(name.as_bytes()),
            };
            let idx = idx as usize;
            if let Some(slot) = st.slots.get_mut(idx) {
                // Every write of a compiled variable of an attached frame
                // (the top level of an included file, a function that used
                // `extract()`) comes here: a scalar is copied and the old
                // scalar forgotten inline, without the out-of-line
                // `Value::clone` and drop glue.
                if let Some(old) = slot.replace(crate::call::clone_value(v)) {
                    if matches!(
                        old,
                        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
                    ) {
                        std::mem::forget(old);
                    } else {
                        crate::gc::release_duplicate(old);
                    }
                }
            }
        }
    }

    /// The slot index of compiled variable `cv`.
    #[inline(always)]
    pub fn cv_idx(&self, cv: u32) -> usize {
        cv as usize
    }

    /// Looks at compiled variable `cv` in place (php-src's `EX_VAR(cv)`):
    /// `read` gets the slot's value (`None` = undefined), and what it
    /// returns cannot borrow from it.
    #[inline(always)]
    #[allow(clippy::inline_always)]
    pub(crate) fn peek_cv<R>(&self, cv: u32, read: impl FnOnce(Option<&Value>) -> R) -> R {
        let idx = cv as usize;
        read(self.cvs.get(idx).and_then(CvSlot::get))
    }

    /// `$cv[i]` looked up for a 64-bit integer index on a packed array:
    /// zero-overhead fast path avoiding `ArrayKey` conversion or hashing.
    #[inline(always)]
    pub(crate) fn peek_cv_dim_int(&self, cv: u32, i: i64) -> Option<Value> {
        let idx = cv as usize;
        let slot = self.cvs.get(idx).and_then(CvSlot::get)?;
        let guard;
        let arr = match slot {
            Value::Array(a) => a,
            Value::Reference(r) => {
                guard = r.try_borrow()?;
                match &*guard {
                    Value::Array(a) => a,
                    _ => return None,
                }
            }
            _ => return None,
        };
        let val = arr.get_packed(i)?;
        Some(val.deref())
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
#[allow(dead_code)]
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
    pub frames: FrameStack,
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
    /// Set of included files (`EG(included_files)`), probed by every
    /// `include_once`/`require_once`: hashed with [`FxHasher`], as the
    /// VM's name tables are, not SipHash (paths are the request's own,
    /// not chosen keys an attacker can collide).
    ///
    /// [`FxHasher`]: rustc_hash::FxHasher
    pub included_set: HashSet<ZStr, rustc_hash::FxBuildHasher>,
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
    /// `EG(active)`: false once the executor has shut down
    /// (`shutdown_executor()`, see [`Vm::ini_deactivate`]) and in a VM
    /// that only judges INI values between requests. A fatal error then
    /// records and prints no backtrace (GHSA-2fjv-p558-g82m), and a
    /// `php_error_docref()` origin is `PHP Request Shutdown` (or `Unknown`
    /// before the first request).
    pub executor_active: bool,
    /// `zend_memory_usage(true)` when the request's accounting ended: what
    /// the request still holds while [`Vm::ini_deactivate`] restores the
    /// memory limits (C frees the last error message only after that).
    pub(crate) end_usage: usize,
    /// `EG_FLAGS_IN_SHUTDOWN`: `php_request_shutdown()` has begun (the
    /// shutdown functions, destructors, extension shutdowns and resource
    /// closing that follow). A docref error raised then with no function
    /// running comes from "PHP Request Shutdown".
    pub request_ending: bool,
    /// Exceptions/bailouts raised inside runtime callbacks.
    pub pending_throw: Option<Throw>,
    /// The last user output handler call threw (`retval` UNDEF), even if
    /// the user exception handler took the exception at shutdown.
    pub(crate) output_handler_threw: bool,
    /// The main script path.
    pub script_path: ZStr,
    /// `SG(request_info)`: the request as the SAPI describes it.
    pub request_info: php_sapi::RequestInfo,
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
    /// What a builtin the tape loop called threw (`exec::hot::THREW`),
    /// until `Vm::run` takes it.
    pub(crate) tape_throw: Option<Throw>,
    /// Set by `op_do_call_nodiscard` when it already reported a native
    /// function's deprecation (`call_builtin_impl` consumes it).
    pub(crate) builtin_deprecation_reported: bool,
    /// Names of registered extensions.
    pub loaded_extensions: Vec<String>,
    /// Request shutdown hooks of extensions.
    pub shutdown_hooks: Vec<fn(&mut Vm)>,
    /// Bailouts so far (fatal errors that end the script): a module's
    /// `RSHUTDOWN` that bails out ends the process (`zend_deactivate_modules`).
    pub bailouts: u32,
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
    /// INI warnings raised while the extensions registered their
    /// directives (`zend_register_ini_entries` in `MINIT`, e.g. an invalid
    /// quantity given with `-d`); the SAPI reports them once at startup.
    pub module_diagnostics: Vec<php_support::Diagnostic>,
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
    /// The include the opener hook is running (frame depth, kind, name):
    /// `php_verror` names `kind()` while the opline is on
    /// `ZEND_INCLUDE_OR_EVAL` and no function runs above it.
    pub(crate) including: Option<(usize, &'static str, ZStr)>,
    /// The `__call`/`__callStatic` frame entered with a `memory_limit`
    /// violation pending: backtraces show it as the trampoline it still is.
    pub(crate) trampoline_entry: Option<usize>,
    /// The compiled variable of the running function whose generic write
    /// walk works on a private one-slot table instead of the function's
    /// symbol table (see `Vm::with_cv_symtab`).
    pub(crate) cv_root: Option<crate::place::CvRoot>,
    /// The private table kept for the next [`Vm::cv_root`].
    pub(crate) cv_scratch: Option<Rc<RefCell<VarStore>>>,
    /// The resolution of the running builtin's callable argument, made by
    /// its parameter check, for `call_user_func[_array]()` to take.
    pub(crate) checked_callable: Option<(Value, PendingCall)>,
    /// `global $name` slots by the address of the name literal (see
    /// `Vm::global_ref_lit`): (address, slot of the global table).
    pub(crate) global_slot_cache: [(usize, u32); crate::place::GLOBAL_SLOT_CACHE],
    /// Source filter hook.
    pub source_filter: Option<crate::ext::SourceFilterFn>,
    /// Which scripts the source filter may rewrite.
    pub source_filter_probe: Option<crate::ext::SourceFilterProbeFn>,
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
    /// Active `__get`/`__set`/`__isset`/`__unset` guards and the
    /// readonly reinitialization markers of a clone in progress
    /// ([`PropGuards`]).
    pub prop_guards: PropGuards,
    /// Request-local observer initialization and callback state.
    pub(crate) observer: crate::observer::ObserverState,
    /// `zend_set_user_opcode_handler()` hooks: Zend opcode name and the
    /// handler run before each such opcode compiled after registration.
    pub(crate) user_opcode_handlers: Vec<(&'static str, crate::observer::UserOpcodeHandler)>,
    /// Dropped last (fields drop in declaration order), after the units and
    /// class tables above: releases this thread's kept class links that
    /// only the dropped VM's scripts held.  Read by nothing: the guard
    /// works through its `Drop`.
    #[allow(dead_code)]
    pub(crate) linked_guard: crate::classlink::LinkedGuard,
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vm")
            .field("frames", &self.frames.len())
            .finish_non_exhaustive()
    }
}

impl Vm {
    /// `zend_rebuild_symbol_table()` for native code that reads or writes
    /// the variables of frame `idx` by name (`extract()`, `compact()`,
    /// `$http_response_header`, `zend_set_local_var()`): attaches the
    /// frame's symbol table and copies its compiled variables into it.
    /// After writing, call [`Vm::sync_symbol_table_writes`].
    pub fn rebuild_symbol_table(&mut self, idx: usize) -> Option<Rc<RefCell<VarStore>>> {
        let f = self.frames.get_mut(idx)?;
        f.attach_symbol_table();
        f.sync_cvs_to_symtab();
        Some(f.vars.clone())
    }

    /// Refreshes the compiled variables of every frame attached to the global
    /// symbol table (the main script and top-level `include`/`eval` frames,
    /// which share it) after a write to it, as php-src's `IS_INDIRECT` slots
    /// would show the new value. Those frames form the bottom of the stack,
    /// so the walk stops at the first frame that is not one of them.
    ///
    /// The running frame is skipped: its compiled variables are live (a
    /// `foreach` value or a fast-path assignment writes them without
    /// mirroring the table), and callers that wrote the table refresh it
    /// themselves. Refreshing it here let `global $x` in an included file
    /// replace the loop variable with the table's stale copy.
    pub(crate) fn sync_global_frames(&mut self) {
        let globals = self.globals.clone();
        let running = self.frames.len().saturating_sub(1);
        for f in self
            .frames
            .iter_mut()
            .take(running)
            .take_while(|f| f.kind == FrameKind::Main || Rc::ptr_eq(&f.vars, &globals))
        {
            if f.symtab_attached {
                f.sync_from_symbol_table();
            }
        }
    }

    /// [`Vm::rebuild_symbol_table`] for the global symbol table (`&EG(symbol_table)`).
    pub fn rebuild_global_symbol_table(&mut self) -> Rc<RefCell<VarStore>> {
        self.attach_globals_symbol_table();
        self.globals.clone()
    }

    /// Makes writes that native code made to the symbol table `vars`
    /// visible to the compiled variables of every frame attached to it
    /// (php-src's attached tables hold `IS_INDIRECT` slots into the CVs).
    pub fn sync_symbol_table_writes(&mut self, vars: &Rc<RefCell<VarStore>>) {
        for f in &mut self.frames {
            if f.symtab_attached && Rc::ptr_eq(&f.vars, vars) {
                f.sync_from_symbol_table();
            }
        }
    }

    /// Whether the main script's frame is the newest frame of the global
    /// scope: no top-level `include` or `eval` runs above it (those frames
    /// share the global table and sit right above it, see
    /// [`Vm::sync_global_frames`]). Only then are the main frame's compiled
    /// variables the global variables' current values. php-src's symbol
    /// table points at the slots of the global-scope frame that is running
    /// (`zend_attach_symbol_table()` on entering an included file or an
    /// `eval`, `zend_detach_symbol_table()` on leaving it): while one runs,
    /// its variables are the current ones, mirrored into the table, and the
    /// main frame's are older until it resumes.
    #[inline]
    pub(crate) fn main_is_global_top(&self) -> bool {
        match self.frames.vec.get(1) {
            Some(f) if self.frames.live > 1 => {
                !std::ptr::eq(Rc::as_ptr(&f.vars), Rc::as_ptr(&self.globals))
            }
            _ => true,
        }
    }

    /// The index of the newest frame of the global scope: the main
    /// script's, or a top-level `include`/`eval` above it sharing the
    /// global table. Those frames form the bottom of the stack (see
    /// [`Vm::sync_global_frames`]). `None` when there is none.
    #[inline]
    fn global_scope_top(&self) -> Option<usize> {
        let globals = Rc::as_ptr(&self.globals);
        self.frames
            .iter()
            .take_while(|f| f.kind == FrameKind::Main || std::ptr::eq(Rc::as_ptr(&f.vars), globals))
            .count()
            .checked_sub(1)
    }

    /// Attaches the global symbol table to the newest frame of the global
    /// scope ([`Vm::global_scope_top`]) if it is unattached, and syncs
    /// that frame's CV slots into `self.globals`, before the table is read
    /// or written by name.
    ///
    /// Only the newest one holds values the table may lack (a write of a
    /// fast path that is not mirrored): each frame below it synchronised
    /// its variables when it started the `include` or `eval` above it
    /// (`Vm::op_include`) and has not run since, so its compiled variables
    /// can only be older than the table. Copying the main frame's in here
    /// while a top-level `eval()` ran above it put the main script's old
    /// values back over the ones the `eval` had just written, which a
    /// function called from it then read through `$GLOBALS`.
    #[inline]
    pub(crate) fn attach_globals_symbol_table(&mut self) {
        let Some(top) = self.global_scope_top() else {
            return;
        };
        if let Some(f) = self.frames.get_mut(top) {
            if !f.symtab_attached {
                f.attach_symbol_table();
            }
            f.sync_cvs_to_symtab();
        }
    }

    /// `ZEND_BIND_STATIC` of static variable `idx` into variable `cv` of
    /// the current frame once the variable was initialised (the op's
    /// branch that binds the stored reference and jumps over the
    /// initialiser: `Op::BindStatic`'s `skip`), without the generic arm's
    /// handle copies: the reference from the function's store
    /// (`UserFunction::statics`, or a closure's own, `Frame::statics`),
    /// bound as `Vm::cv_bind` binds it. The tape runs it in place
    /// (`exec/tape.rs`, `run_vm_op`). `false`, having changed nothing,
    /// when the slot is not initialised yet (or its store is in use): the
    /// generic op runs.
    #[inline(never)]
    pub(crate) fn bind_static_cached(&mut self, cv: u32, idx: u32) -> bool {
        let r = {
            let Some(f) = self.frames.last() else {
                return false;
            };
            let found = match (&f.statics, &f.ufunc) {
                (Some(s), _) => s
                    .try_borrow()
                    .ok()
                    .and_then(|b| b.get(idx as usize).cloned().flatten()),
                (None, Some(uf)) => uf
                    .statics
                    .try_borrow()
                    .ok()
                    .and_then(|b| b.get(idx as usize).cloned().flatten()),
                (None, None) => None,
            };
            let Some(r) = found else {
                return false;
            };
            r
        };
        self.cv_bind(cv, Value::Reference(r));
        true
    }

    /// `ZEND_BIND_GLOBAL` of `global $name` (`lit`, the name's literal in
    /// the running code) into compiled variable `cv` of a function frame,
    /// when its run-time cache slot is warm: the global slot the literal
    /// found last time (`Vm::global_slot_cache`, filled by
    /// `Vm::global_ref_lit`) still has that name and already holds a
    /// reference, and the global table is attached to the newest frame of
    /// the global scope ([`Vm::global_scope_top`], "the main frame" below:
    /// the main script's, or a top-level `include`/`eval`). The reference
    /// is bound as it is, as php-src binds `Z_REF_P` of the cached slot.
    ///
    /// The generic bind ([`Vm::attach_globals_symbol_table`]) first copies
    /// every compiled variable of the main frame into the global table,
    /// one comparison and possibly one store per variable on each
    /// `global` statement. Only this name's slot matters for the bind, and
    /// it is up to date when the main frame's own variable of that name
    /// is undefined (the copy skips it) or holds the same reference (the
    /// copy would change nothing); the other names stay as stale as the
    /// table is between any two generic accesses, each of which refreshes
    /// it first. `false`, nothing changed, otherwise: the generic bind
    /// runs.
    pub(crate) fn bind_global_cached(&mut self, cv: u32, lit: u32) -> bool {
        if self.frames.len() < 2 {
            return false;
        }
        let Some(top) = self.global_scope_top() else {
            return false;
        };
        let r = {
            // The frame the generic bind would synchronise.
            let (Some(main), Some(cur)) = (self.frames.get(top), self.frames.last()) else {
                return false;
            };
            if !main.symtab_attached
                || !Rc::ptr_eq(&main.vars, &self.globals)
                || !std::ptr::eq(main.symtab_slots_of, Rc::as_ptr(&main.vars))
            {
                return false;
            }
            let Some(Value::Str(name)) = cur.code.literals.get(lit as usize) else {
                return false;
            };
            let bytes = name.as_bytes();
            let addr = bytes.as_ptr() as usize;
            let (ca, ci) =
                self.global_slot_cache[(addr >> 3) & (crate::place::GLOBAL_SLOT_CACHE - 1)];
            if ca != addr {
                return false;
            }
            let idx = ci as usize;
            let Ok(st) = self.globals.try_borrow() else {
                return false;
            };
            if st.names.get(idx).is_none_or(|n| n.as_bytes() != bytes) {
                return false;
            }
            let Some(Some(Value::Reference(r))) = st.slots.get(idx) else {
                return false;
            };
            // The main frame's variable of this name, if it has one: the
            // one whose recorded slot is `idx` (none recorded: each
            // variable has the slot of its own number).
            let names = &main.code.cv_names;
            let main_cv = if main.symtab_slots.is_empty() {
                (idx < names.len()).then_some(idx)
            } else {
                main.symtab_slots.iter().position(|&s| s as usize == idx)
            };
            if let Some(i) = main_cv {
                if names.get(i).is_none_or(|n| n.as_bytes() != bytes) {
                    return false;
                }
                match main.cvs.get(i).and_then(CvSlot::get) {
                    None => {}
                    Some(Value::Reference(m)) if PhpRef::ptr_eq(m, r) => {}
                    Some(_) => return false,
                }
            }
            r.clone()
        };
        self.cv_bind(cv, Value::Reference(r));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, FrameKind};
    use crate::opcode::OpArray;
    use php_runtime::{Obj, PhpRef, Value, ZStr};
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
        let mut f = frame_with(&["a", "b"]);
        f.cvs[0].set(Some(Value::Int(7)));
        assert_eq!(f.peek_cv(0, as_int), Some(7));
        // Undefined.
        assert!(f.peek_cv(1, |s| s.is_none()));
    }

    #[test]
    fn detach_symbol_table_moves_values_back_into_the_cvs() {
        use php_runtime::ZArray;
        let mut f = frame_with(&["a", "b", "c"]);
        let arr = ZArray::from_values([Value::Int(1)]);
        f.cvs[0].set(Some(Value::Array(arr.clone())));
        f.cvs[1].set(Some(Value::Int(2)));
        f.attach_symbol_table();
        // Attached: the table holds a second handle of the array.
        assert_eq!(arr.refcount(), 3);
        // A write through the table (`write_place` on its slot) ...
        f.vars.borrow_mut().slots[1] = Some(Value::Int(5));
        // ... and a variable only the table has.
        let extra = f.vars.borrow_mut().find_or_add(b"extra");
        f.vars.borrow_mut().slots[extra as usize] = Some(Value::Int(9));
        f.detach_symbol_table();
        assert!(!f.symtab_attached);
        // The table's values moved into the CVs; it keeps no duplicate.
        assert_eq!(arr.refcount(), 2);
        assert_eq!(as_int(f.cvs[1].get()), Some(5));
        assert!(f.cvs[2].is_none());
        let st = f.vars.borrow();
        assert!(st.slots[..3].iter().all(Option::is_none));
        assert!(matches!(st.slots[extra as usize], Some(Value::Int(9))));
        drop(st);
        // Detaching again is a no-op; attaching copies the CVs back in.
        f.detach_symbol_table();
        f.attach_symbol_table();
        assert!(matches!(f.vars.borrow().slots[1], Some(Value::Int(5))));
    }

    #[test]
    fn find_cv_uses_the_position_when_the_names_line_up() {
        let f = frame_with(&["a", "b"]);
        let mut st = f.vars.borrow_mut();
        assert_eq!(st.find_cv(1, b"b"), Some(1));
        // Not at that position: looked up by name.
        assert_eq!(st.find_cv(0, b"b"), Some(1));
        assert_eq!(st.find_cv(0, b"zz"), None);
        assert_eq!(st.find_or_add_cv(5, b"zz"), 2);
    }

    #[test]
    fn peek_cv_clones_share_the_string() {
        let mut f = frame_with(&["s"]);
        f.cvs[0].set(Some(Value::Str(ZStr::from("abc"))));
        let v = f.peek_cv(0, |slot: Option<&Value>| slot.cloned());
        assert!(matches!(&v, Some(Value::Str(s)) if s.as_bytes() == b"abc"));
        // The slot still holds its value.
        assert!(f.peek_cv(0, |s| matches!(s, Some(Value::Str(_)))));
    }

    #[test]
    fn drop_pool_frees_released_frames_and_keeps_live_ones() {
        let empty = Rc::new(OpArray::default());
        let mut stack = super::FrameStack::new();
        stack.push(frame_with(&["a"]));
        stack.push(frame_with(&["b"]));
        stack.push(frame_with(&["c"]));
        stack.release_top(&empty);
        stack.release_top(&empty);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.vec.len(), 3);
        let empty_vars = Rc::new(RefCell::new(super::VarStore::default()));
        stack.drop_pool(&empty_vars);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.vec.len(), 1);
        assert_eq!(stack.last().map(|f| f.code.cv_names.len()), Some(1));
        // Nothing pooled: nothing to free.
        stack.drop_pool(&empty_vars);
        assert_eq!(stack.vec.len(), 1);
    }

    #[test]
    fn clear_for_reuse_releases_every_value_and_keeps_slot_counts() {
        let mut f = frame_with(&["a", "b"]);
        let s = ZStr::from("a string held by the frame");
        f.cvs[0].set(Some(Value::Str(s.clone())));
        f.cvs[1].set(Some(Value::Int(3)));
        f.tmps = vec![Value::Str(s.clone()), Value::Float(1.5)];
        f.args.push(Some(Value::Str(s.clone())));
        f.magic_name = Some(s.clone());
        assert_eq!(s.refcount(), 5);
        let empty = Rc::new(OpArray::default());
        f.clear_for_reuse(&empty);
        assert_eq!(s.refcount(), 1);
        assert_eq!(f.cvs.len(), 2);
        assert!(f.cvs.iter().all(super::CvSlot::is_none));
        assert_eq!(f.tmps.len(), 2);
        assert!(f.tmps.iter().all(|t| matches!(t, Value::Null)));
        assert!(f.args.is_empty());
        assert!(f.magic_name.is_none());
        assert!(Rc::ptr_eq(&f.code, &empty));
        // Recycling a recycled frame changes nothing.
        f.clear_for_reuse(&empty);
        assert_eq!(f.cvs.len(), 2);
        assert!(Rc::ptr_eq(&f.code, &empty));
    }

    /// `release_returning` releases what `clear_for_reuse` does (it adds
    /// only the possible-root walk, which records nothing while the root
    /// buffer is protected, as in a fresh thread), and so does
    /// `release_top_returning` for the top frame, leaving the one below.
    #[test]
    fn release_returning_releases_like_clear_for_reuse() {
        let empty = Rc::new(OpArray::default());
        let s = ZStr::from("a string held by the frame");
        let arr = php_runtime::ZArray::from_values(vec![Value::Int(1), Value::Str(s.clone())]);
        let mut f = frame_with(&["a", "b", "c"]);
        f.cvs[0].set(Some(Value::Str(s.clone())));
        f.cvs[1].set(Some(Value::Array(arr)));
        f.cvs[2].set(Some(Value::Reference(PhpRef::new(Value::Int(2)))));
        f.tmps = vec![Value::Str(s.clone())];
        assert_eq!(s.refcount(), 4);
        f.release_returning(None, &empty);
        assert_eq!(s.refcount(), 1);
        assert_eq!(f.cvs.len(), 3);
        assert!(f.cvs.iter().all(super::CvSlot::is_none));
        assert!(f.tmps.iter().all(|t| matches!(t, Value::Null)));
        assert!(Rc::ptr_eq(&f.code, &empty));

        let mut stack = super::FrameStack::new();
        let mut below = frame_with(&["x"]);
        below.cvs[0].set(Some(Value::Str(s.clone())));
        stack.push(below);
        let mut top = frame_with(&["y"]);
        top.cvs[0].set(Some(Value::Str(s.clone())));
        stack.push(top);
        assert_eq!(s.refcount(), 3);
        stack.release_top_returning(&empty);
        assert_eq!(stack.len(), 1);
        assert_eq!(s.refcount(), 2);
        assert!(stack.last().is_some_and(|f| f.cvs[0].get().is_some()));
        stack.release_top_returning(&empty);
        assert_eq!(stack.len(), 0);
        assert_eq!(s.refcount(), 1);
        // Nothing live: nothing to release.
        stack.release_top_returning(&empty);
        assert_eq!(stack.len(), 0);
    }

    /// `release_cv` releases one handle of each kind: the shared ones keep
    /// their value with one count less, the last ones free it.
    #[test]
    fn release_cv_releases_shared_and_last_handles() {
        let s = ZStr::from("a string held twice, not interned");
        super::release_cv(Value::Str(s.clone()));
        assert_eq!(s.refcount(), 1);
        let a = php_runtime::ZArray::from_values(vec![Value::Str(s.clone())]);
        assert_eq!(s.refcount(), 2);
        super::release_cv(Value::Array(a.clone()));
        assert_eq!(a.refcount(), 1);
        super::release_cv(Value::Array(a));
        assert_eq!(s.refcount(), 1);
        let r = PhpRef::new(Value::Str(s.clone()));
        super::release_cv(Value::Reference(r.clone()));
        assert_eq!(r.refcount(), 1);
        super::release_cv(Value::Reference(r));
        assert_eq!(s.refcount(), 1);
        let o = Obj::dummy("C");
        let w = o.clone();
        super::release_cv(Value::Object(o));
        assert_eq!(w.refcount(), 1);
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Int(3),
            Value::Float(0.5),
        ] {
            super::release_cv(v);
        }
    }

    fn user_function(code: Rc<OpArray>) -> Rc<crate::func::UserFunction> {
        Rc::new(crate::func::UserFunction {
            name: ZStr::from("f"),
            params: Vec::new(),
            required: 0,
            return_type: None,
            flags: 0,
            code,
            line_start: 1,
            line_end: 1,
            doc_comment: None,
            statics: std::cell::RefCell::new(Vec::new()),
            uses: Vec::new(),
            class_name: None,
            attributes: Vec::new(),
            hook_prop: None,
            call_shape: crate::func::CallShape::default(),
        })
    }

    /// `release_direct` marks the frame ready for the next call of the same
    /// function (`calls::ready_frame` then sets nothing up), in exactly
    /// the state `is_released_for` describes; any other readying or
    /// release clears the mark.
    #[test]
    fn release_direct_marks_the_frame_ready_for_the_same_function() {
        let empty = Rc::new(OpArray::default());
        let code = Rc::new(OpArray::default());
        let uf = user_function(code.clone());
        let other = user_function(Rc::new(OpArray::default()));
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_call(&code, FrameKind::Function);
        f.func = Some(crate::func::Func::User(uf.clone()));
        f.ufunc = Some(uf.clone());
        assert!(!f.released_direct);
        // Only a frame the tape's protocol entered is released directly.
        assert!(!f.release_direct());
        assert!(!f.released_direct);
        f.direct = true;
        f.direct_argc = 1;
        f.ret = super::RetTarget::Tmp(0);
        f.ip = 3;
        assert!(f.release_direct());
        assert!(f.released_direct);
        assert!(f.is_released_for(&uf));
        assert!(!f.is_released_for(&other));
        f.reinit_same_call(&code, FrameKind::Function);
        assert!(!f.released_direct);
        f.direct = true;
        assert!(f.release_direct());
        assert!(f.released_direct);
        f.reinit_for_call(&code, FrameKind::Function);
        assert!(!f.released_direct);
        f.direct = true;
        assert!(f.release_direct());
        f.clear_for_reuse(&empty);
        assert!(!f.released_direct);
        f.reinit_for_call(&code, FrameKind::Function);
        f.func = Some(crate::func::Func::User(uf.clone()));
        f.ufunc = Some(uf.clone());
        f.direct = true;
        assert!(f.release_direct());
        f.reinit_for_builtin(&empty);
        assert!(!f.released_direct);
    }

    /// `release_returning` of a directly entered frame releases every
    /// variable in one pass (scalars, a string, a shared array, a shared
    /// reference, each released to its other holders' count), the
    /// temporaries and the extra arguments, and leaves the frame in the
    /// state `release_direct` leaves (`is_released_for`).
    #[test]
    fn release_returning_releases_a_direct_frame_in_one_pass() {
        let empty = Rc::new(OpArray::default());
        let code = Rc::new(OpArray {
            cv_names: ["i", "f", "s", "a", "r", "u"]
                .iter()
                .map(|n| ZStr::from(*n))
                .collect(),
            num_tmps: 1,
            ..OpArray::default()
        });
        let uf = user_function(code.clone());
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_call(&code, FrameKind::Function);
        f.func = Some(crate::func::Func::User(uf.clone()));
        f.ufunc = Some(uf.clone());
        f.direct = true;
        f.direct_argc = 3;
        f.ret = super::RetTarget::Tmp(0);
        f.ip = 4;
        let s = ZStr::from("a string held by the frame");
        let arr = php_runtime::ZArray::from_values(vec![Value::Int(1), Value::Str(s.clone())]);
        let held = Value::Array(arr);
        let r = PhpRef::new(Value::Int(2));
        f.cvs[0].set(Some(Value::Int(7)));
        f.cvs[1].set(Some(Value::Float(1.5)));
        f.cvs[2].set(Some(Value::Str(s.clone())));
        f.cvs[3].set(Some(held.clone()));
        f.cvs[4].set(Some(Value::Reference(r.clone())));
        f.tmps[0] = Value::Str(s.clone());
        f.args.push(Some(Value::Str(s.clone())));
        assert_eq!(s.refcount(), 5);
        assert_eq!(r.refcount(), 2);
        f.release_returning(None, &empty);
        assert_eq!(s.refcount(), 2, "the array keeps its element");
        assert_eq!(r.refcount(), 1);
        assert!(matches!(&held, Value::Array(a) if a.refcount() == 1));
        assert!(f.cvs.iter().all(super::CvSlot::is_none));
        assert!(f.tmps.iter().all(Value::is_null));
        assert!(f.args.is_empty());
        assert!(f.released_direct);
        assert!(f.is_released_for(&uf));
        drop(held);
        assert_eq!(s.refcount(), 1);
    }

    /// A frame a prepared call readied and never entered (the call turned
    /// back into a pending one) is marked released for that function when
    /// it keeps the function; with anything the readying left behind, or a
    /// function nothing else holds, it is released in full.
    #[test]
    fn release_unentered_marks_a_readied_frame_released_for_its_function() {
        let empty = Rc::new(OpArray::default());
        let code = Rc::new(OpArray {
            cv_names: vec![ZStr::from("a"), ZStr::from("b")],
            num_tmps: 2,
            ..OpArray::default()
        });
        let uf = user_function(code.clone());
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_call(&code, FrameKind::Function);
        f.func = Some(crate::func::Func::User(uf.clone()));
        f.ufunc = Some(uf.clone());
        // An argument sent, then taken back by the turned-back call.
        f.cvs[0].set(Some(Value::Int(1)));
        drop(f.cvs[0].as_mut().take());
        f.release_unentered(&empty);
        assert!(f.released_direct);
        assert!(f.is_released_for(&uf));
        // A frame that went further than readying (a result target set):
        // the full release.
        f.reinit_same_call(&code, FrameKind::Function);
        f.ret = super::RetTarget::Tmp(1);
        let s = ZStr::from("held");
        f.tmps[1] = Value::Str(s.clone());
        f.release_unentered(&empty);
        assert!(!f.released_direct);
        assert_eq!(f.ret, super::RetTarget::Discard);
        assert_eq!(s.refcount(), 1);
        // A function only the frame holds is let go.
        f.reinit_same_call(&code, FrameKind::Function);
        drop(uf);
        f.release_unentered(&empty);
        assert!(!f.released_direct);
        assert!(f.ufunc.is_none() && f.func.is_none());
        assert!(Rc::ptr_eq(&f.code, &empty));
    }

    #[test]
    fn clear_for_reuse_keeps_the_handles_of_a_function_held_elsewhere() {
        let empty = Rc::new(OpArray::default());
        let code = Rc::new(OpArray::default());
        let uf = user_function(code.clone());
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_call(&code, FrameKind::Function);
        f.func = Some(crate::func::Func::User(uf.clone()));
        f.ufunc = Some(uf.clone());
        f.clear_for_reuse(&empty);
        // Held by `uf` too: the frame keeps the function and its code.
        assert!(f.ufunc.as_ref().is_some_and(|u| Rc::ptr_eq(u, &uf)));
        assert!(Rc::ptr_eq(&f.code, &code));
        assert_eq!(Rc::strong_count(&uf), 3);
        // Calling it again in the slot replaces nothing.
        f.reinit_same_call(&code, FrameKind::Function);
        assert!(Rc::ptr_eq(&f.code, &code));
        // Held by nothing else: the frame lets it go.
        drop(uf);
        f.clear_for_reuse(&empty);
        assert!(f.func.is_none() && f.ufunc.is_none());
        assert!(Rc::ptr_eq(&f.code, &empty));
        assert_eq!(Rc::strong_count(&code), 1);
    }

    #[test]
    fn a_kept_function_keeps_its_shared_names_and_another_code_replaces_them() {
        let empty = Rc::new(OpArray::default());
        let named = |n: &str| {
            Rc::new(OpArray {
                cv_names: vec![ZStr::from(n)],
                ..OpArray::default()
            })
        };
        let code = named("a");
        let uf = user_function(code.clone());
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_call(&code, FrameKind::Function);
        f.func = Some(crate::func::Func::User(uf.clone()));
        f.ufunc = Some(uf.clone());
        // `code`, `uf.code` and `f.code`: the names are shared only by a
        // symbol table's attach.
        assert_eq!(Rc::strong_count(&code), 3);
        f.attach_symbol_table();
        assert_eq!(Rc::strong_count(&code), 4);
        f.clear_for_reuse(&empty);
        assert_eq!(Rc::strong_count(&code), 4);
        // The same function again: nothing to share anew.
        f.reinit_for_call(&code, FrameKind::Function);
        assert_eq!(Rc::strong_count(&code), 4);
        f.clear_for_reuse(&empty);
        // Another function of the same shape: its own names.
        let other = named("b");
        f.reinit_same_call(&other, FrameKind::Function);
        f.cvs[0].set(Some(Value::Int(7)));
        f.attach_symbol_table();
        assert_eq!(
            f.vars.borrow().names.first().map(ZStr::as_bytes),
            Some(&b"b"[..])
        );
        assert_eq!(Rc::strong_count(&code), 2);
    }

    #[test]
    fn argument_records_are_lent_whole_and_the_pool_keeps_eight() {
        let code = Rc::new(OpArray::default());
        let mut bufs = super::CallBufs::prewarmed(&code);
        let prewarmed = bufs.args_pool.len();
        assert_eq!(prewarmed, 4);
        // Calls nested ten deep: the prewarmed records, then new ones.
        let mut held: Vec<_> = (0..10).map(|_| bufs.take_args()).collect();
        assert!(bufs.args_pool.is_empty());
        for a in &held {
            assert!(a.values.is_empty() && a.values.capacity() >= 8);
            assert!(a.passed.is_empty() && a.passed.capacity() >= 8);
        }
        // Back in the reverse order; the pool keeps eight.
        while let Some(a) = held.pop() {
            bufs.put_args(a);
        }
        assert_eq!(bufs.args_pool.len(), super::ARGS_POOL);
        // A record goes back with the capacity it has.
        let mut a = bufs.take_args();
        a.values.push(Value::Int(1));
        a.passed.push(true);
        a.release_in_place();
        let ptr = a.values.as_ptr();
        bufs.put_args(a);
        assert_eq!(bufs.take_args().values.as_ptr(), ptr);
    }

    #[test]
    fn lent_call_buffers_go_home_and_nested_calls_take_pooled_ones() {
        let code = Rc::new(OpArray::default());
        let mut bufs = super::CallBufs::prewarmed(&code);
        let pooled = bufs.values.len();
        // The first call borrows the lent pair.
        let outer = bufs.take_values();
        let outer_flags = bufs.take_passed();
        assert_eq!(outer.capacity(), 8);
        assert_eq!(bufs.lent_values.capacity(), 0);
        assert_eq!(bufs.values.len(), pooled);
        // A builtin called meanwhile (a callback's) takes a pooled pair.
        let inner = bufs.take_values();
        let inner_flags = bufs.take_passed();
        assert_eq!(bufs.values.len(), pooled - 1);
        assert_eq!(bufs.passed.len(), pooled - 1);
        // The inner one returns first, and goes home; the outer one then
        // finds home taken and goes to the pool.
        bufs.put_values(inner);
        bufs.put_passed(inner_flags);
        assert_eq!(bufs.lent_values.capacity(), 8);
        bufs.put_values(outer);
        bufs.put_passed(outer_flags);
        assert_eq!(bufs.values.len(), pooled);
        assert_eq!(bufs.passed.len(), pooled);
        // A vector a handler swapped for an empty one is not kept.
        let kept = bufs.take_values();
        bufs.put_values(Vec::new());
        assert_eq!(bufs.lent_values.capacity(), 0);
        assert_eq!(bufs.take_values().capacity(), 8);
        drop(kept);
        // Flags come back cleared.
        let mut flags = bufs.take_passed();
        flags.extend([true, false]);
        bufs.put_passed(flags);
        assert!(bufs.take_passed().is_empty());
    }

    #[test]
    fn clear_native_args_releases_every_argument_front_to_back() {
        let s = ZStr::from("a sent string");
        // (Not `ZArray::new()`: the empty array is one shared table, which
        // has a holder of its own.)
        let a = php_runtime::ZArray::from_values([Value::Int(1)]);
        let held = a.clone();
        let mut args = Vec::with_capacity(8);
        args.push(Some(Value::Str(s.clone())));
        args.push(Some(Value::Array(held)));
        args.push(Some(Value::Int(4)));
        args.push(None);
        assert_eq!(a.refcount(), 2);
        super::clear_native_args(&mut args);
        assert!(args.is_empty());
        assert_eq!(args.capacity(), 8);
        assert_eq!(s.refcount(), 1);
        assert_eq!(a.refcount(), 1);
        // The last handle of an array is released too.
        args.push(Some(Value::Array(php_runtime::ZArray::new())));
        super::clear_native_args(&mut args);
        assert!(args.is_empty());
    }

    #[test]
    fn clear_arg_buffer_releases_extra_arguments_and_keeps_the_capacity() {
        let s = ZStr::from("an extra argument");
        let mut args = Vec::with_capacity(8);
        args.push(None);
        args.push(Some(Value::Str(s.clone())));
        args.push(Some(Value::Int(4)));
        assert_eq!(s.refcount(), 2);
        super::clear_arg_buffer(&mut args);
        assert!(args.is_empty());
        assert_eq!(args.capacity(), 8);
        assert_eq!(s.refcount(), 1);
        // An oversized buffer is let go.
        let mut big: Vec<Option<Value>> = Vec::with_capacity(512);
        big.push(None);
        super::clear_arg_buffer(&mut big);
        assert!(big.is_empty());
        assert_eq!(big.capacity(), 0);
    }

    #[test]
    fn frame_stack_reuses_frames_in_place() {
        let code = Rc::new(OpArray::default());
        let mut stack = super::FrameStack::new();
        let idx0 = stack.push_in_place(&code, FrameKind::Function);
        assert_eq!(idx0, 0);
        assert_eq!(stack.len(), 1);
        stack.release_top(&code);
        assert_eq!(stack.len(), 0);
        let idx1 = stack.push_in_place(&code, FrameKind::Function);
        assert_eq!(idx1, 0);
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn builtin_frames_reuse_pooled_frames_and_keep_the_pool_above() {
        let empty = Rc::new(OpArray::default());
        let empty_vars = Rc::new(RefCell::new(super::VarStore::default()));
        let code = Rc::new(OpArray {
            cv_names: vec![ZStr::from("a"), ZStr::from("b")],
            num_tmps: 3,
            ..OpArray::default()
        });
        let mut stack = super::FrameStack::new();
        stack.push_in_place(&code, FrameKind::Function);
        stack.push_in_place(&code, FrameKind::Function);
        stack.release_top(&empty);
        stack.release_top(&empty);
        assert_eq!(stack.vec.len(), 2);
        let held = ZStr::from("this");
        {
            let f = stack.push_builtin(&empty, &empty_vars);
            assert_eq!(f.kind, FrameKind::Builtin);
            assert!(f.cvs.is_empty() && f.tmps.is_empty());
            assert!(Rc::ptr_eq(&f.code, &empty));
            assert!(f.ufunc.is_none());
            f.args.push(Some(Value::Str(held.clone())));
        }
        assert_eq!(stack.len(), 1);
        stack.release_builtin_top(&empty, &empty_vars);
        assert_eq!(stack.len(), 0);
        assert_eq!(held.refcount(), 1);
        // The pooled frame above the builtin's is still there.
        assert_eq!(stack.vec.len(), 2);
        assert_eq!(stack.vec[1].cvs.len(), 2);
        // A user call in the builtin's frame gets the right slots back.
        let idx = stack.push_in_place(&code, FrameKind::Function);
        assert_eq!(idx, 0);
        assert_eq!(stack.vec[0].cvs.len(), 2);
        assert_eq!(stack.vec[0].tmps.len(), 3);
    }

    #[test]
    fn a_frame_released_by_a_builtin_gets_its_own_variable_store() {
        let empty = Rc::new(OpArray::default());
        let empty_vars = Rc::new(RefCell::new(super::VarStore::default()));
        let mut stack = super::FrameStack::new();
        // A fresh builtin frame shares `empty_vars`.
        assert!(Rc::ptr_eq(
            &stack.push_builtin(&empty, &empty_vars).vars,
            &empty_vars
        ));
        stack.release_builtin_top(&empty, &empty_vars);
        assert!(Rc::ptr_eq(&stack.vec[0].vars, &empty_vars));
        // A function without variables or temporaries passes the size
        // checks of `push_in_place` and must not write into the shared store.
        let code = Rc::new(OpArray::default());
        stack.push_in_place(&code, FrameKind::Function);
        assert!(!Rc::ptr_eq(&stack.vec[0].vars, &empty_vars));
        assert_eq!(Rc::strong_count(&stack.vec[0].vars), 1);
    }

    #[test]
    fn drop_pool_gives_back_what_a_live_builtin_frame_kept() {
        let empty = Rc::new(OpArray::default());
        let empty_vars = Rc::new(RefCell::new(super::VarStore::default()));
        let code = Rc::new(OpArray {
            cv_names: vec![ZStr::from("a")],
            num_tmps: 2,
            ..OpArray::default()
        });
        let mut stack = super::FrameStack::new();
        stack.push_in_place(&code, FrameKind::Function);
        stack.release_top(&empty);
        stack.push_builtin(&empty, &empty_vars);
        assert!(stack.vec[0].tmps.capacity() > 0);
        stack.drop_pool(&empty_vars);
        let f = &stack.vec[0];
        assert_eq!(f.tmps.capacity(), 0);
        assert_eq!(f.cvs.capacity(), 0);
        assert!(Rc::ptr_eq(&f.vars, &empty_vars));
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

    /// Two functions of different shapes alternating in one pooled frame
    /// (the next call at a depth calling another function): each entry
    /// cuts or grows the slot vectors to the code's counts, keeps their
    /// buffers, and finds every slot released; the variable names follow
    /// the code that attaches a table.
    #[test]
    fn reinit_for_call_fits_the_slots_of_alternating_functions() {
        let empty = Rc::new(OpArray::default());
        let big = Rc::new(OpArray {
            cv_names: ["a", "b", "c", "d"].map(ZStr::from).to_vec(),
            num_tmps: 6,
            num_iters: 2,
            num_finally: 1,
            ..OpArray::default()
        });
        let small = Rc::new(OpArray {
            cv_names: vec![ZStr::from("x")],
            num_tmps: 2,
            ..OpArray::default()
        });
        let mut f = Frame::new_pooled(empty.clone());
        for round in 0..3 {
            f.reinit_for_call(&big, FrameKind::Function);
            assert_eq!(
                (f.cvs.len(), f.tmps.len(), f.iters.len(), f.fins.len()),
                (4, 6, 2, 1)
            );
            assert!(f.slots_released());
            let tmps_at = f.tmps.as_ptr();
            let s = ZStr::from(format!("v{round}").as_str());
            f.tmps[5] = Value::Str(s.clone());
            f.cvs[3].set(Some(Value::Str(s.clone())));
            f.attach_symbol_table();
            assert_eq!(
                f.vars
                    .borrow()
                    .names
                    .iter()
                    .map(ZStr::as_bytes)
                    .collect::<Vec<_>>(),
                [&b"a"[..], b"b", b"c", b"d"]
            );
            f.clear_for_reuse(&empty);
            assert_eq!(s.refcount(), 1);
            assert!(f.slots_released());
            f.reinit_for_call(&small, FrameKind::Function);
            assert_eq!(
                (f.cvs.len(), f.tmps.len(), f.iters.len(), f.fins.len()),
                (1, 2, 0, 0)
            );
            assert_eq!(f.tmps.as_ptr(), tmps_at);
            assert!(f.slots_released());
            assert!(Rc::ptr_eq(&f.code, &small));
            {
                let vars = f.vars.borrow();
                assert!(vars.slots.is_empty() && vars.names.is_empty());
            }
            f.cvs[0].set(Some(Value::Int(round)));
            f.attach_symbol_table();
            {
                let vars = f.vars.borrow();
                assert_eq!(vars.names.len(), 1);
                assert_eq!(vars.slots.len(), 1);
                assert!(matches!(vars.slots[0], Some(Value::Int(r)) if r == round));
            }
            f.clear_for_reuse(&empty);
        }
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
        f.reinit_for_call(&code, FrameKind::Function);
        assert_eq!(f.tmps.len(), 3);
        assert_eq!(f.iters.len(), 1);
        assert_eq!(f.fins.len(), 1);
        assert_eq!(f.kind, FrameKind::Function);
        assert!(Rc::ptr_eq(&f.code, &code));
        {
            // The names wait for a symbol table's attach.
            let vars = f.vars.borrow();
            assert!(vars.slots.is_empty());
            assert!(vars.names.is_empty());
        }
        f.attach_symbol_table();
        assert_eq!(f.vars.borrow().names.len(), 2);
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

/// The property guards in effect: php-src's `zend_get_property_guard`
/// (`Zend/zend_object_handlers.c`), which keeps, per object, a flag word
/// for the name of a magic access in progress (`IN_GET`, `IN_SET`,
/// `IN_ISSET`, `IN_UNSET`) and a table only when several names are in
/// flight, plus the `IS_PROP_REINITABLE` state of a readonly property
/// during `__clone()`. Here one list holds (object handle, property name,
/// kind) for every guard in effect, innermost last: a magic access pushes
/// its guard and removes it when the method returns, and only the
/// accesses in progress are on it (a handful at most), so a push, a
/// compare of the handle and kind first, and a removal from the end cost
/// a few instructions where a hash set hashed the name twice.
#[derive(Debug, Default)]
pub struct PropGuards {
    /// The guards in effect, innermost last.
    active: Vec<(u32, ZStr, u8)>,
}

impl PropGuards {
    /// Whether no guard is in effect.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// Whether the guard `kind` of `name` on the object `handle` is in
    /// effect.
    #[inline]
    #[must_use]
    pub fn contains(&self, handle: u32, name: &ZStr, kind: u8) -> bool {
        self.active
            .iter()
            .any(|(h, n, k)| *h == handle && *k == kind && n.as_bytes() == name.as_bytes())
    }

    /// Puts the guard `kind` of `name` on the object `handle` in effect.
    #[inline]
    pub fn insert(&mut self, handle: u32, name: ZStr, kind: u8) {
        self.active.push((handle, name, kind));
    }

    /// Ends the innermost guard `kind` of `name` on the object `handle`
    /// (the last one put in effect: magic accesses nest, and a fiber
    /// suspended inside one may end it out of order). Nothing when none is
    /// in effect.
    #[inline]
    pub fn remove(&mut self, handle: u32, name: &ZStr, kind: u8) {
        if let Some(i) = self
            .active
            .iter()
            .rposition(|(h, n, k)| *h == handle && *k == kind && n.as_bytes() == name.as_bytes())
        {
            self.active.remove(i);
        }
    }

    /// Keeps only the guards `keep` accepts.
    pub fn retain(&mut self, keep: impl FnMut(&(u32, ZStr, u8)) -> bool) {
        self.active.retain(keep);
    }

    /// The guards in effect, outermost first.
    pub fn iter(&self) -> std::slice::Iter<'_, (u32, ZStr, u8)> {
        self.active.iter()
    }

    /// Ends every guard (request shutdown).
    pub fn clear(&mut self) {
        self.active.clear();
    }
}

impl<'a> IntoIterator for &'a PropGuards {
    type Item = &'a (u32, ZStr, u8);
    type IntoIter = std::slice::Iter<'a, (u32, ZStr, u8)>;

    fn into_iter(self) -> Self::IntoIter {
        self.active.iter()
    }
}

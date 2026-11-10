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
            args: Vec::with_capacity(128),
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
/// identical to `Option<Value>`), providing borrow-free read access
/// and direct scalar stores.
#[repr(transparent)]
pub struct CvSlot(UnsafeCell<Option<Value>>);

impl CvSlot {
    #[inline(always)]
    pub fn new(val: Option<Value>) -> Self {
        Self(UnsafeCell::new(val))
    }

    #[inline(always)]
    pub fn get(&self) -> Option<&Value> {
        unsafe { (*self.0.get()).as_ref() }
    }

    #[inline(always)]
    pub fn get_clone(&self) -> Option<Value> {
        unsafe { (*self.0.get()).clone() }
    }

    #[inline(always)]
    pub fn set(&self, val: Option<Value>) -> Option<Value> {
        unsafe { std::mem::replace(&mut *self.0.get(), val) }
    }

    #[inline(always)]
    pub unsafe fn get_mut_unchecked(&self) -> &mut Option<Value> {
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
                f.reinit_for_call(code.clone(), kind);
            }
        } else {
            self.vec.push(Frame::new(code.clone(), kind));
        }
        self.live += 1;
        idx
    }

    /// Releases the topmost frame in place (retaining it in `vec[live..]` for pool reuse).
    #[inline]
    pub fn release_top(&mut self, empty_code: &Rc<OpArray>) {
        if self.live > 0 {
            let last_idx = self.live - 1;
            self.vec[last_idx].clear_for_reuse(empty_code);
            self.live -= 1;
            if self.vec.len() > self.live + 128 {
                self.vec.truncate(self.live + 128);
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
    }

    /// Clears a frame for pool reuse while retaining its compiled code and buffer sizes.
    ///
    /// Every PHP object, closure, generator, value in CVs and temporaries is dropped
    /// so no observable state or handles survive, but the allocated vectors (`tmps`, `cvs`,
    /// `iters`, `fins`) retain their capacity and element counts so that calling the same
    /// function requires zero reallocation.
    #[inline]
    pub fn clear_for_reuse(&mut self, empty_code: &Rc<OpArray>) {
        for t in &mut self.tmps {
            *t = Value::Null;
        }
        for it in &mut self.iters {
            *it = None;
        }
        for fin in &mut self.fins {
            *fin = FinState::None;
        }
        clear_frame_buffer(&mut self.calls);
        for slot in &mut self.cvs {
            slot.set(None);
        }
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
    }

    /// Re-initializes an existing frame when `cvs` and `tmps` already have the right lengths.
    #[inline(always)]
    pub(crate) fn reinit_same_call(&mut self, code: &Rc<OpArray>, kind: FrameKind) {
        self.kind = kind;
        self.ip = 0;
        self.calls.clear();
        self.silence_saved.clear();
        self.symtab_attached = false;
        self.code = code.clone();
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            for slot in vs.slots.iter_mut() {
                *slot = None;
            }
        }
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
        let ncv = code.cv_names.len();
        refill_with(&mut self.cvs, ncv, || CvSlot::new(None));
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            vs.slots.clear();
            vs.names.share(&code);
            vs.index = None;
        } else {
            let mut vs = VarStore::for_code(&code);
            vs.slots.clear();
            self.vars = Rc::new(RefCell::new(vs));
        }
        self.symtab_attached = false;
        self.code = code;
    }

    /// Re-initializes a pooled frame for a new user function call, with
    /// every compiled variable undefined.
    pub fn reinit_for_user(&mut self, code: Rc<OpArray>, kind: FrameKind) {
        self.reinit_for_call(code.clone(), kind);
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
        let mut cvs = Vec::with_capacity(ncv);
        {
            let mut st = vars.borrow_mut();
            for name in &code.cv_names {
                let val = if symtab_attached {
                    let slot = st.find_or_add(name.as_bytes()) as usize;
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
        for (i, name) in self.code.cv_names.iter().enumerate() {
            let slot = st.find_or_add_cv(i, name.as_bytes()) as usize;
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
        let mut st = self.vars.borrow_mut();
        for (i, name) in self.code.cv_names.iter().enumerate() {
            let Some(idx) = st.find_cv(i, name.as_bytes()) else {
                continue;
            };
            let Some(sv) = st.slots.get_mut(idx as usize) else {
                continue;
            };
            let val = sv.take();
            if let Some(cv) = self.cvs.get(i) {
                if let Some(old) = cv.set(val) {
                    crate::gc::release_duplicate(old);
                }
            }
        }
    }

    /// Synchronizes CV slots from the attached symbol table (`zend_detach_symbol_table`).
    pub fn sync_from_symbol_table(&mut self) {
        if !self.symtab_attached {
            return;
        }
        let mut st = self.vars.borrow_mut();
        for (i, name) in self.code.cv_names.iter().enumerate() {
            if let Some(slot) = st.find_cv(i, name.as_bytes()) {
                let val = st.slots.get(slot as usize).cloned().flatten();
                if let Some(cv) = self.cvs.get_mut(i) {
                    if let Some(old) = cv.set(val) {
                        crate::gc::release_duplicate(old);
                    }
                }
            }
        }
    }

    /// Synchronizes all CV slots into the attached symbol table.
    pub fn sync_cvs_to_symtab(&self) {
        if !self.symtab_attached {
            return;
        }
        let mut st = self.vars.borrow_mut();
        for (i, name) in self.code.cv_names.iter().enumerate() {
            if let Some(slot) = st.find_cv(i, name.as_bytes()) {
                let slot = slot as usize;
                if slot < st.slots.len() {
                    // A CV that is `None` was never assigned in this frame
                    // (e.g. cleared by `clear_cv_for_symtab_slot` ahead of an
                    // in-place mutation of the shared slot); it must not wipe
                    // a live global slot that another frame still owns.
                    if let Some(val) = self.cvs.get(i).and_then(CvSlot::get_clone) {
                        if let Some(old) = std::mem::replace(&mut st.slots[slot], Some(val)) {
                            crate::gc::release_duplicate(old);
                        }
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
            let mut st = self.vars.borrow_mut();
            let idx = if st
                .names
                .get(cv as usize)
                .is_some_and(|n| n.as_bytes() == name.as_bytes())
            {
                Some(cv)
            } else {
                st.find(name.as_bytes())
            };
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
    pub fn clear_cv_for_symtab_slot(&self, store: &Rc<RefCell<VarStore>>, sym_idx: usize) {
        if self.symtab_attached && Rc::ptr_eq(store, &self.vars) {
            let st = self.vars.borrow();
            if let Some(name) = st.names.get(sym_idx) {
                if self
                    .code
                    .cv_names
                    .get(sym_idx)
                    .is_some_and(|cv_name| cv_name.as_bytes() == name.as_bytes())
                {
                    if let Some(slot) = self.cvs.get(sym_idx) {
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
                    if let Some(slot) = self.cvs.get(cv_idx) {
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
            let mut st = self.vars.borrow_mut();
            let idx = if st
                .names
                .get(cv as usize)
                .is_some_and(|n| n.as_bytes() == name.as_bytes())
            {
                cv
            } else {
                st.find_or_add(name.as_bytes())
            };
            let idx = idx as usize;
            if idx < st.slots.len() {
                if let Some(old) = std::mem::replace(&mut st.slots[idx], Some(v.clone())) {
                    crate::gc::release_duplicate(old);
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
    pub(crate) fn peek_cv<R>(&self, cv: u32, read: fn(Option<&Value>) -> R) -> R {
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

    /// Attaches the symbol table of the main frame if it is unattached,
    /// syncing its CV slots to `self.globals`.
    #[inline]
    pub(crate) fn attach_globals_symbol_table(&mut self) {
        if let Some(f) = self.frames.first_mut() {
            if !f.symtab_attached
                && (f.kind == FrameKind::Main || Rc::ptr_eq(&f.vars, &self.globals))
            {
                f.attach_symbol_table();
            }
            if f.kind == FrameKind::Main || Rc::ptr_eq(&f.vars, &self.globals) {
                f.sync_cvs_to_symtab();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, FrameKind};
    use crate::opcode::OpArray;
    use php_runtime::{Value, ZStr};
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
        let f = frame_with(&["s"]);
        f.cvs[0].set(Some(Value::Str(ZStr::from("abc"))));
        let v = f.peek_cv(0, |slot: Option<&Value>| slot.cloned());
        assert!(matches!(&v, Some(Value::Str(s)) if s.as_bytes() == b"abc"));
        // The slot still holds its value.
        assert!(f.peek_cv(0, |s| matches!(s, Some(Value::Str(_)))));
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

//! The virtual machine: global state, frames and request lifecycle.

use crate::class::Class;
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

/// A variable table (symbol table): compiled variables plus dynamically
/// created ones.
#[derive(Debug, Default)]
pub struct VarStore {
    /// Values (`None` = undefined).
    pub slots: Vec<Option<Value>>,
    /// Names, parallel to `slots`.
    pub names: Vec<ZStr>,
    index: Option<HashMap<ZStr, u32>>,
}

impl VarStore {
    /// A store for a function's compiled variables.
    pub fn with_names(names: &[ZStr]) -> Self {
        VarStore {
            slots: vec![None; names.len()],
            names: names.to_vec(),
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
        self.names.push(z.clone());
        if let Some(m) = &mut self.index {
            m.insert(z, i);
        }
        i
    }

    /// The defined variables as an array (`get_defined_vars`).
    pub fn to_array(&self) -> HashTable {
        let mut t = HashTable::new();
        for (n, v) in self.names.iter().zip(&self.slots) {
            if let Some(v) = v {
                t.insert(php_runtime::ArrayKey::Str(n.clone()), v.clone());
            }
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
    /// Whether INIT_FCALL resolved a statically named call.
    pub direct: bool,
    /// Whether this is a constructor call from `new`.
    pub is_ctor: bool,
    /// Placeholders of a partial application.
    pub pfa: Option<Box<crate::pfa::PfaArgs>>,
    /// Coerce arguments in strict mode (`ZEND_CALL_STRICT_TYPES` for calls
    /// made from Rust, e.g. attribute instantiation).
    pub strict: bool,
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
    },
    /// A generator.
    Generator {
        /// The generator.
        obj: Obj,
        /// First fetch done.
        started: bool,
    },
    /// A native iterator (extension `get_iterator` hook).
    Native(NativeIter, bool),
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
#[derive(Debug)]
pub struct Frame {
    /// The function (None for top-level code).
    pub func: Option<Func>,
    /// The user function (for user frames).
    pub ufunc: Option<Rc<UserFunction>>,
    /// The code.
    pub code: Rc<OpArray>,
    /// Next instruction.
    pub ip: usize,
    /// Variables.
    pub vars: Rc<RefCell<VarStore>>,
    /// CV index → slot index in `vars` (identity when `None`).
    pub cv_map: Option<Rc<[u32]>>,
    /// Temporaries.
    pub tmps: Vec<Value>,
    /// Foreach iterators.
    pub iters: Vec<Option<ForeachIter>>,
    /// Finally states.
    pub fins: Vec<FinState>,
    /// Pending calls.
    pub calls: Vec<PendingCall>,
    /// `$this`.
    pub this: Option<Obj>,
    /// Class scope (`self`).
    pub scope: Option<Rc<Class>>,
    /// Called scope (`static`).
    pub called: Option<Rc<Class>>,
    /// Arguments as passed (positional; `None` = skipped).
    pub args: Vec<Option<Value>>,
    /// Extra named args.
    pub named: Vec<(ZStr, Value)>,
    /// Where the return value goes.
    pub ret: RetTarget,
    /// The closure being executed.
    pub closure: Option<Obj>,
    /// The generator this frame belongs to.
    pub generator: Option<Obj>,
    /// The exception being matched by `Catch` ops.
    pub pending_exc: Option<Obj>,
    /// Frame kind.
    pub kind: FrameKind,
    /// Static variable storage for this call (closures have their own).
    pub statics: Option<Rc<RefCell<Vec<Option<PhpRef>>>>>,
    /// Whether this is a constructor call (`new` result is the object).
    pub is_ctor: bool,
    /// For include frames: the result temporary in the caller.
    pub silence_saved: Vec<i64>,
    /// Builtin being executed (for backtraces).
    pub builtin_name: Option<ZStr>,
    /// Original method name for `__call` frames.
    pub magic_name: Option<ZStr>,
    /// Whether arguments were passed from strict-types code.
    pub caller_strict: bool,
    /// Whether the call came from internal code (no "called in" suffix).
    pub called_from_internal: bool,
}

/// Keep small call buffers allocation-free without retaining exceptional frame sizes.
fn clear_frame_buffer<T>(buffer: &mut Vec<T>) {
    if buffer.capacity() > 256 {
        *buffer = Vec::new();
    } else {
        buffer.clear();
    }
}

impl Frame {
    /// Creates a pre-allocated empty frame for the pool.
    pub fn new_pooled(empty_code: Rc<OpArray>) -> Self {
        Frame {
            func: None,
            ufunc: None,
            code: empty_code,
            ip: 0,
            vars: Rc::new(RefCell::new(VarStore {
                slots: Vec::with_capacity(8),
                names: Vec::with_capacity(8),
                index: None,
            })),
            cv_map: None,
            tmps: Vec::with_capacity(16),
            iters: Vec::with_capacity(4),
            fins: Vec::with_capacity(2),
            calls: Vec::with_capacity(4),
            this: None,
            scope: None,
            called: None,
            args: Vec::with_capacity(8),
            named: Vec::with_capacity(4),
            ret: RetTarget::Discard,
            closure: None,
            generator: None,
            pending_exc: None,
            kind: FrameKind::Function,
            statics: None,
            is_ctor: false,
            silence_saved: Vec::with_capacity(2),
            builtin_name: None,
            magic_name: None,
            caller_strict: false,
            called_from_internal: false,
        }
    }

    /// Cleans up state when returning to the frame pool, dropping all user values
    /// while preserving ordinary buffer capacities and releasing oversized buffers.
    pub fn clear_for_pool(&mut self, empty_code: &Rc<OpArray>) {
        self.func = None;
        self.ufunc = None;
        self.code = empty_code.clone();
        self.ip = 0;
        self.cv_map = None;
        clear_frame_buffer(&mut self.tmps);
        clear_frame_buffer(&mut self.iters);
        clear_frame_buffer(&mut self.fins);
        clear_frame_buffer(&mut self.calls);
        self.this = None;
        self.scope = None;
        self.called = None;
        clear_frame_buffer(&mut self.args);
        clear_frame_buffer(&mut self.named);
        self.ret = RetTarget::Discard;
        self.closure = None;
        self.generator = None;
        self.pending_exc = None;
        self.statics = None;
        self.is_ctor = false;
        clear_frame_buffer(&mut self.silence_saved);
        self.builtin_name = None;
        self.magic_name = None;
        self.caller_strict = false;
        self.called_from_internal = false;
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            clear_frame_buffer(&mut vs.slots);
            clear_frame_buffer(&mut vs.names);
            vs.index = None;
        } else {
            self.vars = Rc::new(RefCell::new(VarStore::default()));
        }
    }

    /// Re-initializes a pooled frame for a new user function call.
    pub fn reinit_for_user(&mut self, code: Rc<OpArray>, kind: FrameKind) {
        self.code = code;
        self.kind = kind;
        self.ip = 0;
        let ntmps = self.code.num_tmps as usize;
        self.tmps.clear();
        self.tmps.resize(ntmps, Value::Null);
        let niters = self.code.num_iters as usize;
        self.iters.clear();
        self.iters.resize_with(niters, || None);
        let nfin = self.code.num_finally as usize;
        self.fins.clear();
        self.fins.resize(nfin, FinState::None);
        self.calls.clear();
        self.silence_saved.clear();
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            vs.slots.clear();
            vs.slots.resize(self.code.cv_names.len(), None);
            vs.names.clear();
            vs.names.extend(self.code.cv_names.iter().cloned());
            vs.index = None;
        } else {
            self.vars = Rc::new(RefCell::new(VarStore::with_names(&self.code.cv_names)));
        }
    }

    /// Creates a frame for `code` with its own variable store.
    pub fn new(code: Rc<OpArray>, kind: FrameKind) -> Self {
        let vars = Rc::new(RefCell::new(VarStore::with_names(&code.cv_names)));
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
            func: None,
            ufunc: None,
            code,
            ip: 0,
            vars,
            cv_map,
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
            this: None,
            scope: None,
            called: None,
            args: Vec::new(),
            named: Vec::new(),
            ret: RetTarget::Discard,
            closure: None,
            generator: None,
            pending_exc: None,
            kind,
            statics: None,
            is_ctor: false,
            silence_saved: Vec::new(),
            builtin_name: None,
            magic_name: None,
            caller_strict: false,
            called_from_internal: false,
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

    /// The current line.
    pub fn line(&self) -> u32 {
        self.code.line(self.ip.saturating_sub(1))
    }
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
    /// Functions by lower-case name.
    pub functions: HashMap<ZStr, Func>,
    /// Classes by lower-case name.
    pub classes: HashMap<ZStr, Rc<Class>>,
    /// Class declaration order (lower-case names) for `get_declared_classes`.
    pub class_order: Vec<ZStr>,
    /// Constants by name.
    pub constants: HashMap<ZStr, Value>,
    /// User-defined constant names in definition order.
    pub user_constants: Vec<ZStr>,
    /// The global variable table.
    pub globals: Rc<RefCell<VarStore>>,
    /// INI settings.
    pub ini: IniRegistry,
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
    /// Autoload callables.
    pub autoloaders: Vec<Value>,
    /// Classes currently being autoloaded.
    pub autoloading: HashSet<ZStr>,
    /// Exit status.
    pub exit_status: i32,
    /// Shutdown functions.
    pub shutdown_functions: Vec<(Value, Vec<Value>)>,
    /// The last error.
    pub last_error: Option<LastError>,
    /// Exceptions/bailouts raised inside runtime callbacks.
    pub pending_throw: Option<Throw>,
    /// The main script path.
    pub script_path: ZStr,
    /// Command line arguments.
    pub argv: Vec<ZStr>,
    /// Class declarations already bound (by address).
    pub bound_decls: HashMap<usize, Rc<Class>>,
    /// Compiled units kept alive.
    pub units: Vec<Rc<CompiledUnit>>,
    /// `precision`.
    pub precision: i64,
    /// `serialize_precision`.
    pub serialize_precision: i64,
    /// Whether we are in shutdown.
    pub in_shutdown: bool,
    /// Nesting depth of user error handler calls.
    pub in_error_handler: u32,
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
    /// Running fibers (innermost last).
    pub fibers: Vec<Obj>,
    /// Destination temporary of the builtin call in progress (fibers).
    pub pending_call_dst: Option<u32>,
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
    pub output_handler_aliases: HashMap<ZStr, crate::ext::OutputHandlerFn>,
    /// `include` opener hook.
    pub include_opener: Option<crate::ext::IncludeOpenerFn>,
    /// Source filter hook.
    pub source_filter: Option<crate::ext::SourceFilterFn>,
    /// `#[\Deprecated]` suffixes (" since X, msg") of native functions,
    /// by lowercase name.
    pub builtin_deprecations: HashMap<ZStr, Vec<u8>>,
    /// Class constants whose `#[\Deprecated]` notice is being built.
    pub(crate) const_deprecation_guard: HashSet<usize>,
    /// Attributes of user constants declared with `const` (by name).
    pub const_attributes: HashMap<ZStr, Rc<Vec<crate::class::AttributeInfo>>>,
    /// File that defined each user constant (`ReflectionConstant::getFileName`).
    pub const_files: HashMap<ZStr, ZStr>,
    /// Deprecated native constants: name → " since X, msg" suffix.
    pub deprecated_constants: HashMap<ZStr, Vec<u8>>,
    /// Shared empty code for builtin frames.
    pub empty_code: Rc<OpArray>,
    /// Shared empty variable store for builtin frames.
    pub empty_vars: Rc<RefCell<VarStore>>,
    /// Active `__get`/`__set`/`__isset`/`__unset` guards: (object handle,
    /// property name, kind).
    pub prop_guards: HashSet<(u32, ZStr, u8)>,
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vm")
            .field("frames", &self.frames.len())
            .finish_non_exhaustive()
    }
}

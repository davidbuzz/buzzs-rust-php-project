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
    /// Whether this is a constructor call from `new`.
    pub is_ctor: bool,
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
            is_ctor: false,
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
        /// The object.
        obj: Obj,
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
    /// An empty iteration (non-iterable subject).
    Empty,
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

impl Frame {
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
            tmps: vec![Value::Null; ntmps],
            iters: (0..niters).map(|_| None).collect(),
            fins: vec![FinState::None; nfin],
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
    pub message: String,
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
    /// Whether output has been flushed to the SAPI at least once.
    pub headers_sent: bool,
    /// Running fibers (innermost last).
    pub fibers: Vec<Obj>,
    /// Destination temporary of the builtin call in progress (fibers).
    pub pending_call_dst: Option<u32>,
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

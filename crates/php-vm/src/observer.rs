// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Function observers. Notifications run with observation disabled so that
//! callbacks may use the engine without recursively observing themselves.

use crate::fiber::FiberState;
use crate::func::Func;
use crate::opcode::{Op, OpArray};
use crate::vm::{Frame, FrameKind, Throw, Vm};
use php_runtime::{Obj, Value, ZStr};
use std::collections::HashMap;
use std::rc::Rc;

/// Call identity and location, independent of the frame's lifetime.
#[derive(Clone, Debug)]
pub struct ObservedFrame {
    /// Function name, or `None` for file/eval execution.
    pub function: Option<ZStr>,
    /// Declaring class.
    pub scope: Option<ZStr>,
    /// Source file.
    pub filename: Option<ZStr>,
    /// The Zend name of the frame's current opline (`EX(opline)`: the
    /// first one at `begin`, the returning one at `end`) for user code,
    /// when it maps onto a Zend opcode.
    pub opcode: Option<&'static str>,
}

/// `zend_get_opcode_name` of the Zend opcode an [`Op`] implements (only
/// the ones that begin or end an op array).
fn zend_opcode_name(op: &Op) -> Option<&'static str> {
    Some(match op {
        Op::InitFCall { .. } | Op::FramelessICall(_) => "ZEND_INIT_FCALL",
        Op::InitDynCall { .. } => "ZEND_INIT_DYNAMIC_CALL",
        Op::InitMethodCall { .. } => "ZEND_INIT_METHOD_CALL",
        Op::InitStaticCall { .. } => "ZEND_INIT_STATIC_METHOD_CALL",
        Op::New { .. } => "ZEND_NEW",
        Op::DoCall { .. } => "ZEND_DO_FCALL",
        Op::Recv { .. } => "ZEND_RECV",
        Op::RecvInit { .. } => "ZEND_RECV_INIT",
        Op::RecvVariadic { .. } => "ZEND_RECV_VARIADIC",
        Op::Return { .. } => "ZEND_RETURN",
        Op::ReturnRef { .. } | Op::ReturnByRefValue { .. } => "ZEND_RETURN_BY_REF",
        Op::GeneratorCreate => "ZEND_GENERATOR_CREATE",
        Op::Yield { .. } | Op::YieldRef { .. } | Op::YieldByRefValue { .. } => "ZEND_YIELD",
        Op::YieldFrom { .. } => "ZEND_YIELD_FROM",
        Op::Include { .. } => "ZEND_INCLUDE_OR_EVAL",
        Op::Echo(_) => "ZEND_ECHO",
        Op::Throw(_) => "ZEND_THROW",
        Op::Exit(_) => "ZEND_EXIT",
        // Builtin calls compiled to opcodes (`zend_try_compile_special_func`).
        Op::Strlen { .. } => "ZEND_STRLEN",
        Op::Count { .. } => "ZEND_COUNT",
        Op::TypeCheck { .. } => "ZEND_TYPE_CHECK",
        Op::GetType { .. } => "ZEND_GET_TYPE",
        Op::GetClass { .. } => "ZEND_GET_CLASS",
        Op::GetCalledClass { .. } => "ZEND_GET_CALLED_CLASS",
        _ => return None,
    })
}

/// A `zend_set_user_opcode_handler()` hook, called with the Zend opcode
/// name before the opcode runs; the opcode then runs as usual
/// (`ZEND_USER_OPCODE_DISPATCH`).
pub type UserOpcodeHandler = fn(&mut Vm, &str);

/// The Zend opcodes the compiler can hook (each is one instruction kind
/// here): `ZEND_ADD` is a non-constant `+`; the silence pair brackets
/// an `@` expression.
const HOOKABLE_OPCODES: &[&str] = &["ZEND_ADD", "ZEND_BEGIN_SILENCE", "ZEND_END_SILENCE"];

/// A fiber context as the fiber observers see it (`zend_fiber_context`,
/// `Zend/zend_fibers.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FiberContext {
    /// Stable identity while the context lives (its address in php-src).
    pub id: usize,
    /// `kind == zend_ce_fiber`: false for the main context.
    pub is_fiber: bool,
    /// `status` at notification time (before the switch updates it).
    pub status: FiberState,
    /// `ZEND_FIBER_FLAG_DESTROYED`.
    pub destroyed: bool,
    /// `ZEND_FIBER_FLAG_THREW`.
    pub threw: bool,
    /// Identity of `fiber->caller` (`None` when it is `NULL`).
    pub caller: Option<usize>,
}

/// Hooks installed during module startup.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::type_complexity)]
pub struct ObserverHooks {
    /// Called once per function or op array; selects begin/end notifications.
    pub init: Option<fn(&mut Vm, &ObservedFrame) -> bool>,
    /// Called before execution, including argument validation.
    pub begin: Option<fn(&mut Vm, &ObservedFrame)>,
    /// Called at return, yield or unwind. An unwind has no return value.
    pub end: Option<fn(&mut Vm, &ObservedFrame, Option<&Value>)>,
    /// Called immediately before a native handler.
    pub internal_enter: Option<fn(&mut Vm, &ObservedFrame)>,
    /// Called immediately after a native handler.
    pub internal_leave: Option<fn(&mut Vm, &ObservedFrame, Option<&Value>)>,
    /// `zend_observer_fiber_init_register`: a fiber context was created.
    pub fiber_init: Option<fn(&mut Vm, &FiberContext)>,
    /// `zend_observer_fiber_switch_register`: called before switching
    /// from the first context to the second.
    pub fiber_switch: Option<fn(&mut Vm, &FiberContext, &FiberContext)>,
    /// `zend_observer_fiber_destroy_register`: a dead fiber context is
    /// being freed.
    pub fiber_destroy: Option<fn(&mut Vm, &FiberContext)>,
    /// `zend_observer_function_declared_register`: a user function was
    /// declared (lowercase name).
    pub function_declared: Option<fn(&mut Vm, &ZStr)>,
    /// `zend_observer_class_linked_register`: a user class was linked
    /// (lowercase name).
    pub class_linked: Option<fn(&mut Vm, &ZStr)>,
}

enum Identity {
    Function(Func),
    File(Rc<OpArray>),
}

impl Identity {
    fn key(&self) -> (u8, usize) {
        match self {
            Self::Function(Func::User(f)) => (0, Rc::as_ptr(f) as usize),
            Self::Function(Func::Builtin(f)) => (1, Rc::as_ptr(f) as usize),
            Self::File(code) => (2, Rc::as_ptr(code) as usize),
        }
    }
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // independent re-entrancy flags
pub(crate) struct ObserverState {
    /// `hooks.is_some()`, as one byte the call path can test cheaply.
    pub(crate) enabled: bool,
    pub(crate) hooks: Option<ObserverHooks>,
    pub(crate) in_hook: bool,
    pub(crate) keep_builtin_frame: bool,
    pub(crate) quiet_builtin: bool,
    initialized: HashMap<(u8, usize), (Identity, bool)>,
    exception: Option<Obj>,
    /// `exit()` is unwinding: `EG(exception)` is the `UnwindExit` object.
    unwind_exit: bool,
}

impl ObserverState {
    /// Lets the table of observed functions go as it grew: its entries
    /// are dropped as usual, its buffer outside the request's memory
    /// count (see `observer_begin_top_slow`).
    pub(crate) fn release_table(&mut self) {
        let mut table = std::mem::take(&mut self.initialized);
        table.clear();
        php_memlimit::persistent(|| drop(table));
    }
}

impl ObservedFrame {
    /// `at_end`: the opline is the one just executed (the return).
    fn from_frame(f: &Frame, at_end: bool) -> Self {
        let opcode = if f.kind == FrameKind::Builtin {
            None
        } else {
            let ip = if at_end { f.ip.saturating_sub(1) } else { f.ip };
            f.code.ops.get(ip).and_then(zend_opcode_name)
        };
        Self {
            opcode,
            function: f.func.as_ref().map(|func| func.name().clone()),
            scope: f
                .func
                .as_ref()
                .and_then(|_| f.scope.as_ref().map(|c| c.name.clone())),
            filename: (f.kind != FrameKind::Builtin).then(|| f.code.filename.clone()),
        }
    }
}

impl Vm {
    /// `zend_set_user_opcode_handler()` (Zend/zend_execute.c): code compiled
    /// from now on runs `handler` before each `opcode` (a Zend opcode name
    /// such as `ZEND_ADD`). Returns false, installing nothing, for opcodes
    /// the compiler cannot hook.
    pub fn set_user_opcode_handler(&mut self, opcode: &str, handler: UserOpcodeHandler) -> bool {
        let Some(&name) = HOOKABLE_OPCODES.iter().find(|n| **n == opcode) else {
            return false;
        };
        self.user_opcode_handlers.retain(|(n, _)| *n != name);
        self.user_opcode_handlers.push((name, handler));
        true
    }

    /// The opcodes [`Self::set_user_opcode_handler`] hooked, for the
    /// compiler.
    pub(crate) fn user_opcodes(&self) -> Vec<&'static str> {
        self.user_opcode_handlers.iter().map(|(n, _)| *n).collect()
    }

    /// `Op::UserOpcode`: the hook of the opcode named by literal `opcode`.
    #[cold]
    #[inline(never)]
    pub(crate) fn run_user_opcode_handler(&mut self, opcode: u32) {
        let name = self.lit_str(opcode);
        let hook = self
            .user_opcode_handlers
            .iter()
            .find(|(n, _)| n.as_bytes() == name.as_bytes())
            .copied();
        if let Some((n, handler)) = hook {
            handler(self, n);
        }
    }

    /// Installs the process's observer hooks before request execution.
    pub fn set_observer(&mut self, hooks: ObserverHooks) {
        self.observer = ObserverState {
            enabled: true,
            hooks: Some(hooks),
            ..ObserverState::default()
        };
        // The JIT refuses to inline calls from now on (an observed call
        // must run the enter/leave hooks, which an inlined call cannot);
        // traces armed with inlined calls disarm through the version
        // bump. (Traces already *running* when the observer arrives
        // finish their current compiled stretch: accepted, the observer
        // is installed before request execution in every real path.)
        crate::exec::tape::jit::OBSERVER_ON.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::func::bump_func_table_version();
    }

    /// Exception currently being reported to an end/leave observer.
    pub fn current_exception(&self) -> Option<Obj> {
        self.observer.exception.clone()
    }

    /// The class of `EG(exception)` while an end/leave observer runs:
    /// the exception's, or `UnwindExit` while `exit()` unwinds.
    pub fn current_exception_class(&self) -> Option<ZStr> {
        match &self.observer.exception {
            Some(e) => Some(e.class_name()),
            None => self.observer.unwind_exit.then(|| ZStr::from("UnwindExit")),
        }
    }

    /// Begin notification for the frame just pushed. Free without observers.
    #[inline]
    pub(crate) fn observer_begin_top(&mut self) {
        if self.observer.enabled {
            self.observer_begin_top_slow();
        }
    }

    #[cold]
    #[inline(never)]
    fn observer_begin_top_slow(&mut self) {
        let Some(hooks) = self.observer.hooks else {
            return;
        };
        if self.observer.in_hook {
            return;
        }
        let Some(frame) = self.frames.last() else {
            return;
        };
        if frame.observer_hidden || frame.observed || frame.kind == FrameKind::ConstExpr {
            return;
        }
        let identity = frame.func.as_ref().map_or_else(
            || Identity::File(frame.code.clone()),
            |f| Identity::Function(f.clone()),
        );
        let key = identity.key();
        let info = ObservedFrame::from_frame(frame, false);
        self.observer.in_hook = true;
        #[allow(clippy::single_match_else)] // the None arm is a multi-line computation
        let selected = match self.observer.initialized.get(&key) {
            Some((_, selected)) => *selected,
            None => {
                let selected = hooks.init.is_some_and(|init| init(self, &info));
                // php-src keeps a function's observer handlers in its
                // run-time cache, which is not the request's memory: the
                // table's growth here is kept out of `memory_get_usage()`
                // too (`unserialize(serialize(...))` left 192 bytes behind
                // with the observer on, which
                // ext/standard/tests/serialize/bug81142.phpt counts).
                php_memlimit::persistent(|| {
                    self.observer.initialized.insert(key, (identity, selected));
                });
                selected
            }
        };
        if selected {
            self.frame_mut().observed = true;
            if let Some(begin) = hooks.begin {
                begin(self, &info);
            }
        }
        self.observer.in_hook = false;
    }

    /// End notification for frame `index`. Free without observers.
    #[inline]
    pub(crate) fn observer_end_frame(
        &mut self,
        index: usize,
        retval: Option<&Value>,
        thrown: Option<&Throw>,
    ) {
        if self.observer.enabled {
            self.observer_end_frame_slow(index, retval, thrown);
        }
    }

    #[cold]
    #[inline(never)]
    fn observer_end_frame_slow(
        &mut self,
        index: usize,
        retval: Option<&Value>,
        thrown: Option<&Throw>,
    ) {
        let Some(frame) = self.frames.get_mut(index) else {
            return;
        };
        if !std::mem::take(&mut frame.observed) {
            return;
        }
        let info = ObservedFrame::from_frame(frame, true);
        let Some(end) = self.observer.hooks.and_then(|h| h.end) else {
            return;
        };
        let saved = self.observer.exception.take();
        self.observer.exception = match thrown {
            Some(Throw::Exception(e)) => Some(e.clone()),
            _ => None,
        };
        let saved_exit = std::mem::replace(
            &mut self.observer.unwind_exit,
            matches!(thrown, Some(Throw::Exit)),
        );
        let in_hook = std::mem::replace(&mut self.observer.in_hook, true);
        end(self, &info, retval);
        self.observer.in_hook = in_hook;
        self.observer.exception = saved;
        self.observer.unwind_exit = saved_exit;
    }

    /// End notifications for every frame above `base` (outermost last).
    #[inline]
    pub(crate) fn observer_unwind(&mut self, base: usize, thrown: Option<&Throw>) {
        if self.observer.enabled {
            for index in (base..self.frames.len()).rev() {
                self.observer_end_frame_slow(index, None, thrown);
            }
        }
    }

    /// Internal enter (`result == None`) or leave notification around a
    /// native handler. Free without observers.
    #[inline]
    pub(crate) fn observer_internal(
        &mut self,
        index: usize,
        result: Option<&Result<Value, Throw>>,
    ) {
        if self.observer.enabled {
            self.observer_internal_slow(index, result);
        }
    }

    #[cold]
    #[inline(never)]
    fn observer_internal_slow(&mut self, index: usize, result: Option<&Result<Value, Throw>>) {
        let Some(hooks) = self.observer.hooks.filter(|_| !self.observer.in_hook) else {
            return;
        };
        let Some(frame) = self.frames.get(index).filter(|f| !f.observer_hidden) else {
            return;
        };
        let info = ObservedFrame::from_frame(frame, false);
        let saved = self.observer.exception.take();
        self.observer.exception = match result {
            Some(Err(Throw::Exception(e))) => Some(e.clone()),
            _ => None,
        };
        let saved_exit = std::mem::replace(
            &mut self.observer.unwind_exit,
            matches!(result, Some(Err(Throw::Exit))),
        );
        self.observer.in_hook = true;
        match result {
            None => {
                if let Some(enter) = hooks.internal_enter {
                    enter(self, &info);
                }
            }
            Some(r) => {
                if let Some(leave) = hooks.internal_leave {
                    leave(self, &info, r.as_ref().ok());
                }
            }
        }
        self.observer.in_hook = false;
        self.observer.exception = saved;
        self.observer.unwind_exit = saved_exit;
    }

    /// `zend_observer_fiber_init_notify` / `zend_observer_fiber_destroy_notify`
    /// (`destroy` selects the latter). Only called while observers are
    /// enabled.
    #[cold]
    pub(crate) fn observer_fiber_lifecycle(&mut self, ctx: &FiberContext, destroy: bool) {
        let Some(hooks) = self.observer.hooks else {
            return;
        };
        let hook = if destroy {
            hooks.fiber_destroy
        } else {
            hooks.fiber_init
        };
        if let Some(hook) = hook {
            let in_hook = std::mem::replace(&mut self.observer.in_hook, true);
            hook(self, ctx);
            self.observer.in_hook = in_hook;
        }
    }

    /// Runs `f` with the declaration observers silenced: opcache installs
    /// preloaded classes into the class table without linking them again,
    /// so `zend_observer_class_linked_notify` never fires for them in a
    /// request (observer_preload.phpt).
    pub(crate) fn without_declared_observer<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let enabled = std::mem::replace(&mut self.observer.enabled, false);
        let r = f(self);
        self.observer.enabled = enabled;
        r
    }

    /// `zend_observer_function_declared_notify` (`class == false`) /
    /// `zend_observer_class_linked_notify`. Free without observers.
    #[inline]
    pub(crate) fn observer_declared(&mut self, lcname: &ZStr, class: bool) {
        if self.observer.enabled {
            self.observer_declared_slow(lcname, class);
        }
    }

    #[cold]
    #[inline(never)]
    fn observer_declared_slow(&mut self, lcname: &ZStr, class: bool) {
        let Some(hooks) = self.observer.hooks else {
            return;
        };
        let hook = if class {
            hooks.class_linked
        } else {
            hooks.function_declared
        };
        if let Some(hook) = hook {
            let in_hook = std::mem::replace(&mut self.observer.in_hook, true);
            hook(self, lcname);
            self.observer.in_hook = in_hook;
        }
    }

    /// `zend_observer_fiber_switch_notify`. Only called while observers are
    /// enabled.
    #[cold]
    pub(crate) fn observer_fiber_switch(&mut self, from: &FiberContext, to: &FiberContext) {
        if let Some(hook) = self.observer.hooks.and_then(|h| h.fiber_switch) {
            let in_hook = std::mem::replace(&mut self.observer.in_hook, true);
            hook(self, from, to);
            self.observer.in_hook = in_hook;
        }
    }
}

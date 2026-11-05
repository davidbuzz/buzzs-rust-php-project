//! The output buffering layer — a port of `main/output.c`.
//!
//! The stack ([`OutputStack`]) lives inside the embedding VM, which
//! implements [`OutputHost`] to give the operations access to it, to the
//! SAPI writer and to user callbacks.  All operations are free functions
//! taking the host, so a user handler running inside
//! [`OutputHost::call_handler`] can re-enter them (`echo` inside a handler,
//! `ob_start()` inside a handler → fatal error), exactly like PHP.
//!
//! The userland functions (`ob_start()`, `ob_get_clean()`, …) are provided
//! with PHP's exact notices; they are reported as docref diagnostics, so the
//! VM prefixes them with the active function name (`ob_end_clean(): …`).

use std::io::Write;

use crate::diag::{Diagnostic, ErrorLevel};

/// Handler operation and ability flags (`PHP_OUTPUT_HANDLER_*`).
pub mod flags {
    /// Standard pass-through write.
    pub const WRITE: i32 = 0x00;
    /// First invocation of a handler.
    pub const START: i32 = 0x01;
    /// The buffer is being cleaned.
    pub const CLEAN: i32 = 0x02;
    /// The buffer is being flushed.
    pub const FLUSH: i32 = 0x04;
    /// Last invocation (handler removed).
    pub const FINAL: i32 = 0x08;
    /// `PHP_OUTPUT_HANDLER_CONT` (alias of `WRITE`).
    pub const CONT: i32 = WRITE;
    /// `PHP_OUTPUT_HANDLER_END` (alias of `FINAL`).
    pub const END: i32 = FINAL;
    /// Internal handler type.
    pub const INTERNAL: i32 = 0x0000;
    /// User handler type.
    pub const USER: i32 = 0x0001;
    /// `ob_clean()` allowed.
    pub const CLEANABLE: i32 = 0x0010;
    /// `ob_flush()` allowed.
    pub const FLUSHABLE: i32 = 0x0020;
    /// `ob_end_*()` allowed.
    pub const REMOVABLE: i32 = 0x0040;
    /// All three abilities.
    pub const STDFLAGS: i32 = 0x0070;
    /// The handler has been started.
    pub const STARTED: i32 = 0x1000;
    /// The handler failed and is disabled.
    pub const DISABLED: i32 = 0x2000;
    /// The handler has processed data.
    pub const PROCESSED: i32 = 0x4000;
    /// The user handler produced output while running.
    pub const PRODUCED_OUTPUT: i32 = 0x8000;

    /// `PHP_OUTPUT_HANDLER_ABILITY_FLAGS()`.
    pub const fn ability(bitmask: i32) -> i32 {
        bitmask & !0xf00f
    }
}

/// Global output status flags (`php_output_get_status()`).
pub mod status {
    /// `ob_implicit_flush()` is on.
    pub const IMPLICITFLUSH: u32 = 0x01;
    /// Output is disabled (headers could not be sent, or by the SAPI).
    pub const DISABLED: u32 = 0x02;
    /// Something has been written to a handler.
    pub const WRITTEN: u32 = 0x04;
    /// Something has been sent to the SAPI.
    pub const SENT: u32 = 0x08;
    /// A handler is active.
    pub const ACTIVE: u32 = 0x10;
    /// A handler is running.
    pub const LOCKED: u32 = 0x20;
    /// The layer is activated for the request.
    pub const ACTIVATED: u32 = 0x10_0000;
}

const POP_FORCE: u32 = 0x001;
const POP_DISCARD: u32 = 0x010;
const POP_SILENT: u32 = 0x100;

/// Name of the default (pass-through) handler.
pub const DEFAULT_HANDLER_NAME: &str = "default output handler";
/// Name of the null handler.
pub const DEVNULL_HANDLER_NAME: &str = "null output handler";

/// An internal (Rust) handler: receives the buffer and the operation mode,
/// returns the output (`Some(empty)` swallows it) or `None` on failure.
pub type InternalHandler = Box<dyn FnMut(&[u8], i32) -> Option<Vec<u8>>>;

enum HandlerFunc<C> {
    /// `php_output_handler_default_func`: passes data through.
    Default,
    /// `php_output_handler_devnull_func`: swallows data.
    DevNull,
    /// A user callback (taken out while it runs).
    User(Option<C>),
    /// A Rust handler (taken out while it runs).
    Internal(Option<InternalHandler>),
}

struct Handler<C> {
    id: u64,
    name: Vec<u8>,
    flags: i32,
    level: i32,
    size: usize,
    buffer: Vec<u8>,
    buffer_size: usize,
    func: HandlerFunc<C>,
}

/// `PHP_OUTPUT_HANDLER_INITBUF_SIZE()`.
fn initbuf_size(s: usize) -> usize {
    if s > 0 {
        (s + 0xfff) & !0xfff
    } else {
        0x4000
    }
}

/// Entry of `ob_get_status()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerStatus {
    /// Handler name (`"default output handler"`, callable name, …).
    pub name: Vec<u8>,
    /// `0` internal, `1` user.
    pub kind: i32,
    /// Full flag bitmask.
    pub flags: i32,
    /// Nesting level (0-based).
    pub level: i32,
    /// Chunk size given to `ob_start()`.
    pub chunk_size: usize,
    /// Allocated buffer size (as PHP computes it).
    pub buffer_size: usize,
    /// Bytes currently buffered.
    pub buffer_used: usize,
}

/// The output handler stack (`OG()` globals).
pub struct OutputStack<C> {
    handlers: Vec<Handler<C>>,
    /// Handler temporarily removed by `php_output_flush()`.
    detached: Option<Handler<C>>,
    active: Option<u64>,
    running: Option<u64>,
    flags: u32,
    next_id: u64,
}

impl<C> std::fmt::Debug for OutputStack<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputStack")
            .field("level", &self.handlers.len())
            .field("active", &self.active)
            .field("running", &self.running)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

impl<C> Default for OutputStack<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> OutputStack<C> {
    /// A new, *activated* stack (`php_output_activate()`).
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            detached: None,
            active: None,
            running: None,
            flags: status::ACTIVATED,
            next_id: 1,
        }
    }

    /// `php_output_activate()`: resets everything for a new request.
    pub fn activate(&mut self) {
        *self = Self::new();
    }

    fn find(&self, id: u64) -> Option<&Handler<C>> {
        self.handlers
            .iter()
            .find(|h| h.id == id)
            .or(self.detached.as_ref().filter(|h| h.id == id))
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut Handler<C>> {
        if let Some(h) = self.handlers.iter_mut().find(|h| h.id == id) {
            return Some(h);
        }
        self.detached.as_mut().filter(|h| h.id == id)
    }

    fn active_handler(&self) -> Option<&Handler<C>> {
        self.active.and_then(|id| self.find(id))
    }

    fn in_stack(&self, id: u64) -> bool {
        self.active.is_some() && self.handlers.iter().any(|h| h.id == id)
    }

    /// `php_output_get_level()`.
    pub fn level(&self) -> usize {
        if self.active.is_some() {
            self.handlers.len()
        } else {
            0
        }
    }

    /// `php_output_get_status()`.
    pub fn status(&self) -> u32 {
        (self.flags
            | if self.active.is_some() {
                status::ACTIVE
            } else {
                0
            }
            | if self.running.is_some() {
                status::LOCKED
            } else {
                0
            })
            & 0xff
    }

    /// `php_output_set_status()` (used by SAPIs to disable output).
    pub fn set_status(&mut self, s: u32) {
        self.flags = (self.flags & !0xf) | (s & 0xf);
    }

    /// Whether the layer is activated.
    pub fn is_activated(&self) -> bool {
        self.flags & status::ACTIVATED != 0
    }

    /// `php_output_set_implicit_flush()`.
    pub fn set_implicit_flush(&mut self, on: bool) {
        if on {
            self.flags |= status::IMPLICITFLUSH;
        } else {
            self.flags &= !status::IMPLICITFLUSH;
        }
    }

    /// `php_output_get_contents()`: `None` when no buffer is active.
    pub fn contents(&self) -> Option<Vec<u8>> {
        self.active_handler().map(|h| h.buffer.clone())
    }

    /// `php_output_get_length()`.
    pub fn length(&self) -> Option<usize> {
        self.active_handler().map(|h| h.buffer.len())
    }

    /// `ob_list_handlers()`: names from the bottom of the stack.
    pub fn list_handlers(&self) -> Vec<Vec<u8>> {
        if self.active.is_none() {
            return Vec::new();
        }
        self.handlers.iter().map(|h| h.name.clone()).collect()
    }

    fn handler_status(h: &Handler<C>) -> HandlerStatus {
        HandlerStatus {
            name: h.name.clone(),
            kind: h.flags & 0xf,
            flags: h.flags,
            level: h.level,
            chunk_size: h.size,
            buffer_size: h.buffer_size,
            buffer_used: h.buffer.len(),
        }
    }

    /// `ob_get_status(false)`: the active handler's status (`None` → `[]`).
    pub fn active_status(&self) -> Option<HandlerStatus> {
        self.active_handler().map(Self::handler_status)
    }

    /// `ob_get_status(true)`: all handlers, bottom up.
    pub fn full_status(&self) -> Vec<HandlerStatus> {
        if self.active.is_none() {
            return Vec::new();
        }
        self.handlers.iter().map(Self::handler_status).collect()
    }

    /// `php_output_handler_started()`.
    pub fn handler_started(&self, name: &[u8]) -> bool {
        self.level() > 0 && self.handlers.iter().any(|h| h.name == name)
    }

    /// `PHP_OUTPUT_HANDLER_HOOK_GET_FLAGS`.
    pub fn running_flags(&self) -> Option<i32> {
        self.running.and_then(|id| self.find(id)).map(|h| h.flags)
    }

    /// `PHP_OUTPUT_HANDLER_HOOK_IMMUTABLE`.
    pub fn set_running_immutable(&mut self) {
        if let Some(h) = self.running.and_then(|id| self.find_mut(id)) {
            h.flags &= !(flags::REMOVABLE | flags::CLEANABLE);
        }
    }

    fn new_handler(
        &mut self,
        name: &[u8],
        chunk_size: usize,
        flags: i32,
        func: HandlerFunc<C>,
    ) -> Handler<C> {
        let id = self.next_id;
        self.next_id += 1;
        Handler {
            id,
            name: name.to_vec(),
            flags,
            level: 0,
            size: chunk_size,
            buffer: Vec::new(),
            buffer_size: initbuf_size(chunk_size),
            func,
        }
    }
}

/// What the embedding VM provides to the output layer.
pub trait OutputHost {
    /// The VM's representation of a user callback.
    type Callback;

    /// The output stack.
    fn output(&mut self) -> &mut OutputStack<Self::Callback>;

    /// Calls a user output handler with `(buffer, mode)`.  Returns `None`
    /// if it returned `false` or the call failed (threw); otherwise its
    /// return value converted to a string (`true` → empty).
    fn call_handler(
        &mut self,
        callback: &mut Self::Callback,
        buffer: &[u8],
        mode: i32,
    ) -> Option<Vec<u8>>;

    /// `sapi_module.ub_write()`.
    fn sapi_write(&mut self, data: &[u8]);

    /// `sapi_flush()`.
    fn sapi_flush(&mut self) {}

    /// `php_output_direct()` — writes made while the layer is not
    /// activated (stdout in the CLI).
    fn direct_write(&mut self, data: &[u8]) {
        self.sapi_write(data);
    }

    /// `php_header()`: sends headers before the first output; returning
    /// `false` disables output.
    fn send_headers(&mut self) -> bool {
        true
    }

    /// Checks the module's conflicts before a handler is pushed.
    fn check_handler_conflicts(&mut self, _name: &[u8]) -> bool {
        true
    }

    /// Refreshes request settings before an internal handler runs.
    fn before_internal_handler(&mut self, _name: &[u8]) {}

    /// Whether a diagnostic has raised an exception in the embedding VM.
    fn has_pending_exception(&self) -> bool {
        false
    }

    /// Whether the last [`call_handler`](Self::call_handler) threw
    /// (`Z_TYPE(retval) == IS_UNDEF` in `php_output_handler_op`), even if
    /// the exception was handled meanwhile (by the user exception handler
    /// when no frame is executing, at request shutdown).
    fn handler_threw(&self) -> bool {
        self.has_pending_exception()
    }

    /// Whether a callback bailed out rather than throwing an exception.
    fn has_pending_fatal(&self) -> bool {
        false
    }

    /// Releases a handler's callable, including its captured objects.
    fn release_handler_callback(&mut self, callback: Self::Callback) {
        drop(callback);
    }

    /// Reports a diagnostic.  An `E_ERROR` is fatal: the operation that
    /// reported it returns and the VM must bail out.
    fn report(&mut self, diagnostic: Diagnostic);
}

/// The way a buffer is created by [`ob_start`].
pub enum StartHandler<C> {
    /// `ob_start()` / `ob_start(null)`: the default handler.
    Default,
    /// A user callable with its display name (`"foo"`, `"C::m"`,
    /// `"{closure:file:line}"`).
    User {
        /// Name shown by `ob_list_handlers()`.
        name: Vec<u8>,
        /// The callable.
        callback: C,
    },
    /// A host callback implementing an internal output handler.
    Alias {
        /// Name shown by `ob_list_handlers()`.
        name: Vec<u8>,
        /// The host's native callback.
        callback: C,
    },
    /// An internal handler (e.g. an alias like `ob_gzhandler`).
    Internal {
        /// Name shown by `ob_list_handlers()`.
        name: Vec<u8>,
        /// The handler.
        func: InternalHandler,
    },
    /// The null handler (`php_output_start_devnull()`).
    DevNull,
    /// Not callable: the warning `zend_is_callable_ex()` produced (e.g.
    /// `"function \"no\" not found or invalid function name"`).
    Invalid(String),
}

impl<C> std::fmt::Debug for StartHandler<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => f.write_str("Default"),
            Self::User { name, .. } => write!(f, "User({})", String::from_utf8_lossy(name)),
            Self::Alias { name, .. } => write!(f, "Alias({})", String::from_utf8_lossy(name)),
            Self::Internal { name, .. } => write!(f, "Internal({})", String::from_utf8_lossy(name)),
            Self::DevNull => f.write_str("DevNull"),
            Self::Invalid(e) => write!(f, "Invalid({e})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Failure,
    Success,
    NoData,
}

/// `php_output_context`.
struct Context {
    op: i32,
    input: Vec<u8>,
    output: Option<Vec<u8>>,
}

impl Context {
    fn new(op: i32) -> Self {
        Self {
            op,
            input: Vec::new(),
            output: None,
        }
    }

    /// `php_output_context_swap()`.
    fn swap(&mut self) {
        self.input = self.output.take().unwrap_or_default();
    }

    /// `php_output_context_pass()`.
    fn pass(&mut self) {
        self.output = Some(std::mem::take(&mut self.input));
    }

    /// `php_output_context_reset()`.
    fn reset(&mut self) {
        self.input.clear();
        self.output = None;
    }
}

fn notice<H: OutputHost + ?Sized>(host: &mut H, msg: String) {
    host.report(Diagnostic::docref(ErrorLevel::NOTICE, msg));
}

/// `php_output_handler_conflict()`: warns if `old` is still on the stack.
pub fn handler_conflict<H: OutputHost + ?Sized>(host: &mut H, new: &[u8], old: &[u8]) -> bool {
    if !host.output().handler_started(old) {
        return false;
    }
    let name = String::from_utf8_lossy(new);
    let message = if new == old {
        format!("Output handler '{name}' cannot be used twice")
    } else {
        format!(
            "Output handler '{name}' conflicts with '{}'",
            String::from_utf8_lossy(old)
        )
    };
    host.report(Diagnostic::docref(ErrorLevel::WARNING, message));
    true
}

/// `php_output_header()`.
fn output_header<H: OutputHost + ?Sized>(host: &mut H) {
    if !host.send_headers() {
        host.output().flags |= status::DISABLED;
    }
}

fn handler_free<H: OutputHost + ?Sized>(host: &mut H, handler: Handler<H::Callback>) {
    if let HandlerFunc::User(Some(callback)) = handler.func {
        host.release_handler_callback(callback);
    }
}

/// `php_output_deactivate()`: drops every handler (end of request, or
/// after a fatal error).
pub fn deactivate<H: OutputHost + ?Sized>(host: &mut H) {
    if host.output().flags & status::ACTIVATED != 0 {
        output_header(host);
        {
            let o = host.output();
            o.flags ^= status::ACTIVATED;
            o.active = None;
            o.running = None;
        }
        while let Some(handler) = host.output().handlers.pop() {
            handler_free(host, handler);
            host.output().active = None;
        }
        if let Some(handler) = host.output().detached.take() {
            handler_free(host, handler);
        }
    }
}

/// `php_output_lock_error()`.
fn lock_error<H: OutputHost + ?Sized>(host: &mut H, op: i32) -> bool {
    let o = host.output();
    if op != 0 && o.active.is_some() && o.running.is_some() {
        deactivate(host);
        host.report(Diagnostic::docref(
            ErrorLevel::ERROR,
            "Cannot use output buffering in output buffering display handlers",
        ));
        return true;
    }
    false
}

/// `php_output_handler_append()`: returns `true` when the handler does not
/// need to run yet.
fn handler_append<C>(o: &mut OutputStack<C>, id: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    o.flags |= status::WRITTEN;
    let running = o.running.is_some();
    let Some(h) = o.find_mut(id) else { return true };
    let free = h.buffer_size.saturating_sub(h.buffer.len());
    if free <= data.len() {
        let grow_int = initbuf_size(h.size);
        let grow_buf = initbuf_size(data.len() - free);
        h.buffer_size += grow_int.max(grow_buf);
    }
    h.buffer.extend_from_slice(data);
    if h.size != 0 && h.buffer.len() >= h.size {
        return running;
    }
    true
}

/// `php_output_handler_op()`.
#[allow(clippy::too_many_lines)]
fn handler_op<H: OutputHost + ?Sized>(host: &mut H, id: u64, ctx: &mut Context) -> Status {
    let original_op = ctx.op;
    {
        let Some(h) = host.output().find(id) else {
            return Status::Failure;
        };
        if h.flags & flags::DISABLED != 0 {
            return Status::Failure;
        }
    }
    if lock_error(host, ctx.op) {
        return Status::Failure;
    }
    let o = host.output();
    if ctx.op == flags::WRITE && o.active.is_some() && o.running.is_some() {
        if let Some(h) = o.find_mut(id) {
            if h.flags & flags::USER != 0 {
                h.flags |= flags::PRODUCED_OUTPUT;
            }
        }
    }
    let input = std::mem::take(&mut ctx.input);
    let stored = handler_append(o, id, &input);
    ctx.input = input;
    if stored && ctx.op == 0 {
        ctx.op = original_op;
        return Status::NoData;
    }

    let mut still_have = true;
    let status;
    {
        let o = host.output();
        let Some(h) = o.find_mut(id) else {
            return Status::Failure;
        };
        if h.flags & flags::STARTED == 0 {
            ctx.op |= flags::START;
        }
        o.running = Some(id);
    }
    let (is_callback, buffer, taken_user, taken_internal, passthrough) = {
        let o = host.output();
        let Some(h) = o.find_mut(id) else {
            return Status::Failure;
        };
        let buffer = h.buffer.clone();
        match &mut h.func {
            HandlerFunc::User(c) => (true, buffer, c.take(), None, None),
            HandlerFunc::Internal(f) => (false, buffer, None, f.take(), None),
            HandlerFunc::Default => (false, buffer, None, None, Some(true)),
            HandlerFunc::DevNull => (false, buffer, None, None, Some(false)),
        }
    };
    let is_user = host
        .output()
        .find(id)
        .is_some_and(|h| h.flags & flags::USER != 0);
    if !is_user {
        let name = host
            .output()
            .find(id)
            .map(|h| h.name.clone())
            .unwrap_or_default();
        host.before_internal_handler(&name);
    }
    if is_callback {
        let retval = match taken_user {
            Some(mut cb) => {
                let r = host.call_handler(&mut cb, &buffer, ctx.op);
                if let Some(Handler {
                    func: HandlerFunc::User(slot),
                    ..
                }) = host.output().find_mut(id)
                {
                    *slot = Some(cb);
                } else {
                    host.release_handler_callback(cb);
                }
                r
            }
            None => None,
        };
        if !host.output().is_activated() || host.has_pending_fatal() {
            // A fatal error inside the handler: PHP bails out here.
            return Status::Failure;
        }
        let produced = host
            .output()
            .find(id)
            .is_some_and(|h| h.flags & flags::PRODUCED_OUTPUT != 0);
        // main/output.c skips the deprecation only when the call threw
        // (retval UNDEF); a handler that returned false still gets it.
        if is_user && produced && (retval.is_some() || !host.handler_threw()) {
            let name = {
                let h = host.output().find_mut(id);
                h.map(|h| {
                    h.flags |= flags::DISABLED;
                    h.flags &= !flags::PRODUCED_OUTPUT;
                    String::from_utf8_lossy(&h.name).into_owned()
                })
                .unwrap_or_default()
            };
            host.report(Diagnostic::docref(
                ErrorLevel::DEPRECATED,
                format!("Producing output from user output handler {name} is deprecated"),
            ));
            let o = host.output();
            still_have = o.in_stack(id);
            if still_have {
                if let Some(h) = o.find_mut(id) {
                    h.flags &= !flags::DISABLED;
                }
            }
        } else if host.output().find(id).is_none() {
            still_have = false;
        }
        status = match retval {
            None => Status::Failure,
            Some(v) if v.is_empty() => Status::NoData,
            Some(v) => {
                ctx.output = Some(v);
                Status::Success
            }
        };
    } else {
        ctx.input.clone_from(&buffer);
        let result = match (passthrough, taken_internal) {
            (Some(true), _) => Some(buffer),
            (Some(false), _) => Some(Vec::new()),
            (None, Some(mut f)) => {
                let r = f(&buffer, ctx.op);
                if !host.output().is_activated() || host.has_pending_fatal() {
                    return Status::Failure;
                }
                if let Some(h) = host.output().find_mut(id) {
                    if let HandlerFunc::Internal(slot) = &mut h.func {
                        *slot = Some(f);
                    }
                }
                r
            }
            (None, None) => None,
        };
        status = match result {
            Some(v) if v.is_empty() => Status::NoData,
            Some(v) => {
                ctx.output = Some(v);
                Status::Success
            }
            None => Status::Failure,
        };
        if host.output().find(id).is_none() {
            still_have = false;
        }
    }
    let o = host.output();
    if still_have {
        if let Some(h) = o.find_mut(id) {
            h.flags |= flags::STARTED;
        }
    }
    o.running = None;
    if !still_have {
        return status;
    }
    let Some(h) = o.find_mut(id) else {
        return status;
    };
    match status {
        Status::Failure => {
            h.flags |= flags::DISABLED;
            ctx.output = Some(std::mem::take(&mut h.buffer));
            h.buffer_size = 0;
        }
        Status::NoData | Status::Success => {
            if status == Status::NoData {
                ctx.reset();
            }
            h.buffer.clear();
            h.flags |= flags::PROCESSED;
        }
    }
    ctx.op = original_op;
    status
}

/// `php_output_stack_apply_op()`: returns `true` to stop the walk.
fn stack_apply_op<H: OutputHost + ?Sized>(host: &mut H, id: u64, ctx: &mut Context) -> bool {
    let (was_disabled, level) = match host.output().find(id) {
        Some(h) => (h.flags & flags::DISABLED != 0, h.level),
        None => return false,
    };
    let status = if was_disabled {
        Status::Failure
    } else {
        handler_op(host, id, ctx)
    };
    let level = host.output().find(id).map_or(level, |h| h.level);
    match status {
        Status::NoData => true,
        Status::Success => {
            if level != 0 {
                ctx.swap();
            }
            false
        }
        Status::Failure => {
            if was_disabled {
                if level == 0 {
                    ctx.pass();
                }
            } else if level != 0 {
                ctx.swap();
            }
            false
        }
    }
}

/// `php_output_op()`: passes data through the handler stack to the SAPI.
fn output_op<H: OutputHost + ?Sized>(host: &mut H, op: i32, data: &[u8]) {
    if lock_error(host, op) {
        return;
    }
    let mut ctx = Context::new(op);
    let o = host.output();
    let count = o.handlers.len();
    if o.active.is_some() && count > 0 {
        ctx.input = data.to_vec();
        if count > 1 {
            let ids: Vec<u64> = o.handlers.iter().rev().map(|h| h.id).collect();
            for id in ids {
                if !host.output().handlers.iter().any(|h| h.id == id) {
                    continue;
                }
                if stack_apply_op(host, id, &mut ctx) {
                    break;
                }
                if !host.output().is_activated() || host.has_pending_fatal() {
                    return;
                }
            }
        } else if let Some(top) = o
            .handlers
            .last()
            .filter(|h| h.flags & flags::DISABLED == 0)
            .map(|h| h.id)
        {
            handler_op(host, top, &mut ctx);
            if !host.output().is_activated() || host.has_pending_fatal() {
                return;
            }
        } else {
            ctx.pass();
        }
    } else {
        ctx.output = Some(data.to_vec());
    }
    if let Some(out) = ctx.output.filter(|o| !o.is_empty()) {
        output_header(host);
        if host.output().flags & status::DISABLED == 0 {
            host.sapi_write(&out);
            if host.output().flags & status::IMPLICITFLUSH != 0 {
                host.sapi_flush();
            }
            host.output().flags |= status::SENT;
        }
    }
}

/// `php_output_write()` — `echo`, `print` and every other output.
/// Returns the number of bytes accepted.
pub fn write<H: OutputHost + ?Sized>(host: &mut H, data: &[u8]) -> usize {
    let flags = host.output().flags;
    if flags & status::ACTIVATED != 0 {
        output_op(host, flags::WRITE, data);
        return data.len();
    }
    if flags & status::DISABLED != 0 {
        return 0;
    }
    host.direct_write(data);
    data.len()
}

/// `php_output_write_unbuffered()`.
pub fn write_unbuffered<H: OutputHost + ?Sized>(host: &mut H, data: &[u8]) -> usize {
    if host.output().flags & status::ACTIVATED != 0 {
        host.sapi_write(data);
    } else {
        host.direct_write(data);
    }
    data.len()
}

/// `php_output_flush()`: flushes the active buffer if it is flushable.
pub fn flush<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    let Some(id) = host
        .output()
        .active_handler()
        .filter(|h| h.flags & flags::FLUSHABLE != 0)
        .map(|h| h.id)
    else {
        return false;
    };
    let mut ctx = Context::new(flags::FLUSH);
    handler_op(host, id, &mut ctx);
    if !host.output().is_activated() || host.has_pending_fatal() {
        return true;
    }
    if let Some(out) = ctx.output.filter(|o| !o.is_empty()) {
        let o = host.output();
        let top = o.handlers.pop();
        o.detached = top;
        write(host, &out);
        let o = host.output();
        if let Some(h) = o.detached.take() {
            o.handlers.push(h);
        }
    }
    true
}

/// `php_output_flush_all()`.
pub fn flush_all<H: OutputHost + ?Sized>(host: &mut H) {
    if host.output().active.is_some() {
        output_op(host, flags::FLUSH, &[]);
    }
}

/// `php_output_clean()`: cleans the active buffer if it is cleanable.
pub fn clean<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    let Some(id) = host
        .output()
        .active_handler()
        .filter(|h| h.flags & flags::CLEANABLE != 0)
        .map(|h| h.id)
    else {
        return false;
    };
    let mut ctx = Context::new(flags::CLEAN);
    handler_op(host, id, &mut ctx);
    true
}

/// `php_output_clean_all()`: cleans every buffer regardless of flags.
pub fn clean_all<H: OutputHost + ?Sized>(host: &mut H) {
    if host.output().active.is_none() {
        return;
    }
    let mut ctx = Context::new(flags::CLEAN);
    let ids: Vec<u64> = host.output().handlers.iter().rev().map(|h| h.id).collect();
    for id in ids {
        if let Some(h) = host.output().find_mut(id) {
            h.buffer.clear();
        } else {
            continue;
        }
        handler_op(host, id, &mut ctx);
        ctx.reset();
    }
}

/// `php_output_stack_pop()`.
fn stack_pop<H: OutputHost + ?Sized>(host: &mut H, pop_flags: u32) -> bool {
    let what = if pop_flags & POP_DISCARD != 0 {
        "discard"
    } else {
        "send"
    };
    let Some((id, hflags, name, level)) = host.output().active_handler().map(|h| {
        (
            h.id,
            h.flags,
            String::from_utf8_lossy(&h.name).into_owned(),
            h.level,
        )
    }) else {
        if pop_flags & POP_SILENT == 0 {
            notice(
                host,
                format!("Failed to {what} buffer. No buffer to {what}"),
            );
        }
        return false;
    };
    if pop_flags & POP_FORCE == 0 && hflags & flags::REMOVABLE == 0 {
        if pop_flags & POP_SILENT == 0 {
            notice(host, format!("Failed to {what} buffer of {name} ({level})"));
        }
        return false;
    }
    let mut ctx = Context::new(flags::FINAL);
    if hflags & flags::DISABLED == 0 {
        if hflags & flags::STARTED == 0 {
            ctx.op |= flags::START;
        }
        if pop_flags & POP_DISCARD != 0 {
            ctx.op |= flags::CLEAN;
        }
        handler_op(host, id, &mut ctx);
        if !host.output().is_activated() {
            return true;
        }
        if host.has_pending_fatal() {
            return false;
        }
    }
    let o = host.output();
    let orphan = o.handlers.pop();
    o.active = o.handlers.last().map(|h| h.id);
    if pop_flags & POP_DISCARD == 0 {
        if let Some(out) = ctx.output.filter(|o| !o.is_empty()) {
            write(host, &out);
        }
    }
    if let Some(handler) = orphan {
        handler_free(host, handler);
    }
    true
}

/// `php_output_end()`: flushes and removes the active buffer.
pub fn end<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    stack_pop(host, 0)
}

/// `php_output_end_all()`: flushes and removes all buffers (shutdown).
pub fn end_all<H: OutputHost + ?Sized>(host: &mut H) {
    // A handler that bailed out (fatal error, frameless `exit`) is still
    // marked running. In php-src the bailout leaves `php_output_end_all()`
    // for good; a second call (a module's RSHUTDOWN ended the buffers
    // before the VM's own shutdown step) must not re-enter the stack and
    // report a spurious lock error.
    if host.output().running.is_some() && host.has_pending_fatal() {
        return;
    }
    while host.output().active.is_some() && stack_pop(host, POP_FORCE) {}
}

/// `php_output_discard()`: discards and removes the active buffer.
pub fn discard<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    stack_pop(host, POP_DISCARD)
}

/// `php_output_discard_all()`.
pub fn discard_all<H: OutputHost + ?Sized>(host: &mut H) {
    while host.output().active.is_some() {
        if !stack_pop(host, POP_DISCARD | POP_FORCE) {
            break;
        }
    }
}

/// `php_output_handler_start()` for a prepared handler.
fn handler_start<H: OutputHost + ?Sized>(host: &mut H, mut handler: Handler<H::Callback>) -> bool {
    if host.output().flags & status::ACTIVATED == 0 {
        handler_free(host, handler);
        return false;
    }
    if lock_error(host, flags::START) {
        handler_free(host, handler);
        return false;
    }
    if !host.check_handler_conflicts(&handler.name) {
        handler_free(host, handler);
        return false;
    }
    let o = host.output();
    handler.level = i32::try_from(o.handlers.len()).unwrap_or(i32::MAX);
    o.active = Some(handler.id);
    o.handlers.push(handler);
    true
}

/// `php_output_start_user()` without the userland notice: starts a buffer.
pub fn start<H: OutputHost + ?Sized>(
    host: &mut H,
    handler: StartHandler<H::Callback>,
    chunk_size: usize,
    handler_flags: i32,
) -> bool {
    let ability = flags::ability(handler_flags);
    let o = host.output();
    let h = match handler {
        StartHandler::Default => o.new_handler(
            DEFAULT_HANDLER_NAME.as_bytes(),
            chunk_size,
            ability | flags::INTERNAL,
            HandlerFunc::Default,
        ),
        StartHandler::DevNull => o.new_handler(
            DEVNULL_HANDLER_NAME.as_bytes(),
            0x4000,
            0,
            HandlerFunc::DevNull,
        ),
        StartHandler::User { name, callback } => o.new_handler(
            &name,
            chunk_size,
            ability | flags::USER,
            HandlerFunc::User(Some(callback)),
        ),
        StartHandler::Alias { name, callback } => o.new_handler(
            &name,
            chunk_size,
            ability | flags::INTERNAL,
            HandlerFunc::User(Some(callback)),
        ),
        StartHandler::Internal { name, func } => o.new_handler(
            &name,
            chunk_size,
            ability | flags::INTERNAL,
            HandlerFunc::Internal(Some(func)),
        ),
        StartHandler::Invalid(err) => {
            host.report(Diagnostic::docref(ErrorLevel::WARNING, err));
            return false;
        }
    };
    handler_start(host, h)
}

/// `ob_start($callback, $chunk_size, $flags)`.
pub fn ob_start<H: OutputHost + ?Sized>(
    host: &mut H,
    handler: StartHandler<H::Callback>,
    chunk_size: i64,
    handler_flags: i64,
) -> bool {
    let chunk = usize::try_from(chunk_size.max(0)).unwrap_or(usize::MAX);
    #[allow(clippy::cast_possible_truncation)]
    let fl = handler_flags as i32;
    let was_activated = host.output().is_activated();
    if start(host, handler, chunk, fl) {
        return true;
    }
    // A lock error is fatal (bailout); PHP never reaches the notice then.
    if (was_activated && !host.output().is_activated()) || host.has_pending_exception() {
        return false;
    }
    notice(host, "Failed to create buffer".into());
    false
}

fn active_name_level<H: OutputHost + ?Sized>(host: &mut H) -> (String, i32) {
    host.output()
        .active_handler()
        .map(|h| (String::from_utf8_lossy(&h.name).into_owned(), h.level))
        .unwrap_or_default()
}

/// `ob_flush()`.
pub fn ob_flush<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    if host.output().active.is_none() {
        notice(host, "Failed to flush buffer. No buffer to flush".into());
        return false;
    }
    if !flush(host) {
        let (n, l) = active_name_level(host);
        notice(host, format!("Failed to flush buffer of {n} ({l})"));
        return false;
    }
    true
}

/// `ob_clean()`.
pub fn ob_clean<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    if host.output().active.is_none() {
        notice(host, "Failed to delete buffer. No buffer to delete".into());
        return false;
    }
    if !clean(host) {
        let (n, l) = active_name_level(host);
        notice(host, format!("Failed to delete buffer of {n} ({l})"));
        return false;
    }
    true
}

/// `ob_end_flush()`.
pub fn ob_end_flush<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    if host.output().active.is_none() {
        notice(
            host,
            "Failed to delete and flush buffer. No buffer to delete or flush".into(),
        );
        return false;
    }
    end(host)
}

/// `ob_end_clean()`.
pub fn ob_end_clean<H: OutputHost + ?Sized>(host: &mut H) -> bool {
    if host.output().active.is_none() {
        notice(host, "Failed to delete buffer. No buffer to delete".into());
        return false;
    }
    discard(host)
}

/// `ob_get_flush()`: `None` is PHP's `false`.
pub fn ob_get_flush<H: OutputHost + ?Sized>(host: &mut H) -> Option<Vec<u8>> {
    let Some(contents) = host.output().contents() else {
        notice(
            host,
            "Failed to delete and flush buffer. No buffer to delete or flush".into(),
        );
        return None;
    };
    if !end(host) && !host.has_pending_fatal() {
        let (n, l) = active_name_level(host);
        notice(host, format!("Failed to delete buffer of {n} ({l})"));
    }
    Some(contents)
}

/// `ob_get_clean()`: `None` is PHP's `false`.
pub fn ob_get_clean<H: OutputHost + ?Sized>(host: &mut H) -> Option<Vec<u8>> {
    let contents = host.output().contents()?;
    if !discard(host) && !host.has_pending_fatal() {
        let (n, l) = active_name_level(host);
        notice(host, format!("Failed to delete buffer of {n} ({l})"));
    }
    Some(contents)
}

/// `ob_get_contents()`.
pub fn ob_get_contents<H: OutputHost + ?Sized>(host: &mut H) -> Option<Vec<u8>> {
    host.output().contents()
}

/// `ob_get_length()`.
pub fn ob_get_length<H: OutputHost + ?Sized>(host: &mut H) -> Option<usize> {
    host.output().length()
}

/// `ob_get_level()`.
pub fn ob_get_level<H: OutputHost + ?Sized>(host: &mut H) -> usize {
    host.output().level()
}

/// `ob_list_handlers()`.
pub fn ob_list_handlers<H: OutputHost + ?Sized>(host: &mut H) -> Vec<Vec<u8>> {
    host.output().list_handlers()
}

/// `ob_implicit_flush($enable)`.
pub fn ob_implicit_flush<H: OutputHost + ?Sized>(host: &mut H, enable: bool) {
    host.output().set_implicit_flush(enable);
}

/// Request startup as `php_request_startup()` does it from the INI
/// settings `output_handler`, `output_buffering` and `implicit_flush`.
pub fn request_startup<H: OutputHost + ?Sized>(
    host: &mut H,
    output_handler: Option<StartHandler<H::Callback>>,
    output_buffering: i64,
    implicit_flush: bool,
) {
    host.output().activate();
    if let Some(h) = output_handler {
        start(host, h, 0, flags::STDFLAGS);
    } else if output_buffering != 0 {
        let chunk = if output_buffering > 1 {
            usize::try_from(output_buffering).unwrap_or(0)
        } else {
            0
        };
        start(host, StartHandler::Default, chunk, flags::STDFLAGS);
    } else if implicit_flush {
        host.output().set_implicit_flush(true);
    }
}

/// Request shutdown: flush all buffers (`php_output_end_all()`), then
/// deactivate the layer (`php_output_deactivate()`).
pub fn request_shutdown<H: OutputHost + ?Sized>(host: &mut H) {
    end_all(host);
    deactivate(host);
}

/// A ready-made host: closure handlers and a [`Write`] sink.
///
/// Handlers cannot re-enter the output layer (use a custom
/// [`OutputHost`] in the VM for that); diagnostics are collected.
pub struct SimpleOutput {
    /// The stack.
    pub stack: OutputStack<InternalHandler>,
    /// Where SAPI output goes.
    pub sink: Box<dyn Write>,
    /// Collected diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

impl std::fmt::Debug for SimpleOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleOutput")
            .field("stack", &self.stack)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

impl SimpleOutput {
    /// A new host writing to `sink`.
    pub fn new(sink: Box<dyn Write>) -> Self {
        Self {
            stack: OutputStack::new(),
            sink,
            diagnostics: Vec::new(),
        }
    }
}

impl OutputHost for SimpleOutput {
    type Callback = InternalHandler;

    fn output(&mut self) -> &mut OutputStack<InternalHandler> {
        &mut self.stack
    }

    fn call_handler(
        &mut self,
        callback: &mut InternalHandler,
        buffer: &[u8],
        mode: i32,
    ) -> Option<Vec<u8>> {
        callback(buffer, mode)
    }

    fn sapi_write(&mut self, data: &[u8]) {
        // A failing sink loses output, like a closed stdout in PHP.
        let _ = self.sink.write_all(data);
    }

    fn sapi_flush(&mut self) {
        let _ = self.sink.flush();
    }

    fn report(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }
}

#[cfg(test)]
mod tests;

//! `ext/libxml`: the `libxml_*()` functions, the `LibXMLError` class, the
//! `LIBXML_*` constants and the per-request libxml state shared by every
//! libxml-based extension.
//!
//! # Shared state API for other XML bindings
//!
//! DOM, `SimpleXML`, `XMLReader`, XSL and SOAP bindings must not keep their
//! own copy of the libxml error list, entity loader or stream context; the
//! state lives in `php_libxml` (thread-local) and this module adapts it to
//! the VM:
//!
//! * [`run`] — wrap **every** call into libxml2 that may parse, load or
//!   write: it lends the VM to the stream layer (user stream wrappers, the
//!   user entity loader and `php://output` need it), emits the queued
//!   stream and libxml diagnostics as `func(): message` warnings/notices
//!   and re-raises an exception thrown by a PHP callback (the entity
//!   loader) — diagnostics are dropped in that case, as PHP does when
//!   `EG(exception)` is set.
//! * [`flush_diagnostics`] — emit queued libxml diagnostics (when a call
//!   was not wrapped in [`run`]).
//! * [`errors_enabled`] — `php_libxml_uses_internal_errors()`.
//! * [`errors`] / [`last_error`] / [`clear_errors`] — the error list.
//! * [`push_error`] — `php_libxml_issue_error()`: list it or warn.
//! * [`new_error_object`] — build a `LibXMLError` object.
//! * [`with_streams_context`] / [`streams_context`] — the context set with
//!   `libxml_set_streams_context()` (or `FG(default_context)`).
//! * [`entity_loader_disabled`] — `libxml_disable_entity_loader()` state.
//!
//! The stream hooks routing libxml2's file I/O through `php-streams` are
//! installed at module startup and on every request startup.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;

use php_ext_streams::{bridge, res};
use php_libxml::error::{self as xerr, Diagnostic, DiagnosticLevel, ErrorProperty, LibXmlError};
use php_libxml::io::{self as xio, EntityLoaderResult, EntityRequest, InputStream, StreamHooks};
use php_runtime::{Obj, Resource, Value, ZArray, ZStr};
use php_streams::context::ContextRef;
use php_streams::stream::Stream;
use php_streams::wrapper::{self, REPORT_ERRORS, URL_STAT_QUIET};
use php_vm::ext::{acc, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::Vm;

/// `E_NOTICE`.
const E_NOTICE: i64 = 8;

thread_local! {
    /// The PHP callable given to `libxml_set_external_entity_loader()`.
    static LOADER: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// The PHP value given to `libxml_set_streams_context()` (kept alive).
    static CONTEXT: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Names of the native functions inside [`run`] (innermost last), for
    /// messages built outside `php_error_docref`.
    static ACTIVE: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------
// Public state API
// ---------------------------------------------------------------------

/// Whether `libxml_use_internal_errors(true)` is in effect.
pub fn errors_enabled() -> bool {
    xerr::uses_internal_errors()
}

/// The recorded errors (`libxml_get_errors()`); empty unless internal
/// errors are enabled.
pub fn errors() -> Vec<LibXmlError> {
    xerr::get_errors()
}

/// The last error (`libxml_get_last_error()`).
pub fn last_error() -> Option<LibXmlError> {
    xerr::get_last_error()
}

/// `libxml_clear_errors()`.
pub fn clear_errors() {
    xerr::clear_errors();
}

/// `php_libxml_issue_error(E_WARNING/E_NOTICE, msg)`: appends to the error
/// list when internal errors are enabled, otherwise emits
/// `func(): msg`.
pub fn push_error(vm: &mut Vm, level: DiagnosticLevel, message: &[u8]) -> VmResult<()> {
    xerr::issue_error(level, message);
    flush_diagnostics(vm)
}

/// Emits the libxml diagnostics queued so far as `func(): message`
/// warnings (or notices). They are discarded while a PHP callback's
/// exception is pending.
pub fn flush_diagnostics(vm: &mut Vm) -> VmResult<()> {
    let diagnostics = xerr::take_diagnostics();
    if bridge::has_pending() {
        return Ok(());
    }
    for d in diagnostics {
        emit(vm, &d)?;
    }
    Ok(())
}

fn emit(vm: &mut Vm, d: &Diagnostic) -> VmResult<()> {
    match d.level {
        DiagnosticLevel::Warning => vm.php_warning_bytes(&d.message),
        DiagnosticLevel::Notice => vm.php_error_bytes(E_NOTICE, &d.message),
    }
}

/// Runs a libxml2 operation of the native function described by `a`.
///
/// The VM is reachable from stream wrappers and the user entity loader
/// while `f` runs; afterwards stream warnings and libxml diagnostics are
/// emitted and an exception thrown by a callback is returned as `Err`.
pub fn run<R>(vm: &mut Vm, a: &Args, f: impl FnOnce() -> R) -> VmResult<R> {
    ensure_hooks();
    ACTIVE.with(|s| s.borrow_mut().push(a.func_name()));
    let r = bridge::run(vm, f);
    ACTIVE.with(|s| s.borrow_mut().pop());
    match r {
        Ok(v) => {
            flush_diagnostics(vm)?;
            Ok(v)
        }
        Err(t) => {
            xerr::take_diagnostics();
            Err(t)
        }
    }
}

/// The stream context libxml2 opens files with: the one set by
/// `libxml_set_streams_context()`, else `FG(default_context)`.
pub fn streams_context() -> Option<ContextRef> {
    if let Some(ctx) = xio::streams_context().and_then(|c| c.downcast_ref::<ContextRef>().cloned())
    {
        return Some(ctx);
    }
    res::ctx_cell_of(&res::default_context()).map(|c| Rc::clone(&c.ctx))
}

/// Calls `f` with [`streams_context`].
pub fn with_streams_context<R>(f: impl FnOnce(Option<&ContextRef>) -> R) -> R {
    let ctx = streams_context();
    f(ctx.as_ref())
}

/// Whether `libxml_disable_entity_loader(true)` is in effect.
pub fn entity_loader_disabled() -> bool {
    xio::entity_loader_disabled()
}

/// Builds a `LibXMLError` object (`php_libxml_create_error_object`).
pub fn new_error_object(vm: &mut Vm, error: &LibXmlError) -> VmResult<Value> {
    let class = vm
        .find_class(b"LibXMLError")
        .ok_or_else(|| vm.throw("Error", "Class \"LibXMLError\" not found"))?;
    let obj: Obj = vm.new_object(&class)?;
    for (name, value) in error.properties() {
        let v = match value {
            ErrorProperty::Int(i) => Value::Int(i),
            ErrorProperty::Str(s) => Value::Str(ZStr::from(s)),
        };
        vm.set_property_raw(&obj, name, v);
    }
    Ok(Value::Object(obj))
}

// ---------------------------------------------------------------------
// Stream hooks
// ---------------------------------------------------------------------

/// A `php-streams` stream read by libxml2 (closed when dropped).
struct StreamReader(Stream);

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0
            .read(buf)
            .map_err(|_| std::io::Error::other("stream read failed"))
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A `php-streams` stream written by libxml2 (closed when dropped).
struct StreamWriter(Stream);

impl Write for StreamWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .write(buf)
            .map_err(|_| std::io::Error::other("stream write failed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush();
        Ok(())
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Reads a stream resource without owning it (entity loader results).
pub(crate) struct ResourceReader {
    cell: Rc<res::StreamCell>,
    _keep: Resource,
}

impl Read for ResourceReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.cell.get() {
            Some(mut s) => s
                .read(buf)
                .map_err(|_| std::io::Error::other("stream read failed")),
            None => Err(std::io::Error::other("stream closed")),
        }
    }
}

/// `php_libxml_streams_IO_open_read_wrapper`.
fn open_read(path: &[u8]) -> Option<InputStream> {
    let ctx = streams_context();
    if let Some(loc) = wrapper::locate(path, 0) {
        if let Some(Err(())) = loc
            .wrapper
            .url_stat(&loc.path, URL_STAT_QUIET, ctx.as_ref())
        {
            return None;
        }
    }
    let mut opened = None;
    let stream =
        php_streams::open::open_wrapper(path, "rb", REPORT_ERRORS, &mut opened, ctx.as_ref())?;
    Some(InputStream {
        reader: Box::new(StreamReader(stream)),
        charset: None,
    })
}

/// `php_libxml_streams_IO_open_write_wrapper`.
fn open_write(path: &[u8]) -> Option<Box<dyn Write>> {
    let ctx = streams_context();
    let mut opened = None;
    let stream =
        php_streams::open::open_wrapper(path, "wb", REPORT_ERRORS, &mut opened, ctx.as_ref())?;
    Some(Box::new(StreamWriter(stream)))
}

thread_local! {
    static HOOKS_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs the `php-streams` I/O hooks on this thread (idempotent).
pub fn ensure_hooks() {
    if HOOKS_INSTALLED.with(std::cell::Cell::get) {
        return;
    }
    HOOKS_INSTALLED.with(|h| h.set(true));
    xio::set_stream_hooks(Some(StreamHooks {
        open_read: Some(Box::new(open_read)),
        open_write: Some(Box::new(open_write)),
    }));
}

// ---------------------------------------------------------------------
// User entity loader
// ---------------------------------------------------------------------

/// `zend_get_callable_name()` for messages.
fn callable_display_name(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string_lossy(),
        Value::Array(a) => {
            let first = a.get_int(0).map(Value::deref);
            let second = a.get_int(1).map(Value::deref);
            let class = match first {
                Some(Value::Object(o)) => o.class_name().to_string_lossy(),
                Some(Value::Str(s)) => s.to_string_lossy(),
                _ => String::new(),
            };
            let method = match second {
                Some(Value::Str(s)) => s.to_string_lossy(),
                _ => String::new(),
            };
            format!("{class}::{method}")
        }
        Value::Object(o) => format!("{}::__invoke", o.class_name().to_string_lossy()),
        Value::Reference(r) => callable_display_name(&r.get()),
        _ => String::new(),
    }
}

/// `function_handler->common.function_name` of a callable.
fn callable_function_name(v: &Value) -> Vec<u8> {
    match v {
        Value::Str(s) => {
            let b = s.as_bytes();
            match b.windows(2).position(|w| w == b"::") {
                Some(p) => b[p + 2..].to_vec(),
                None => b.strip_prefix(b"\\").unwrap_or(b).to_vec(),
            }
        }
        Value::Array(a) => match a.get_int(1).map(Value::deref) {
            Some(Value::Str(s)) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        },
        Value::Object(o) if o.class_name().as_bytes().eq_ignore_ascii_case(b"Closure") => {
            b"{closure}".to_vec()
        }
        Value::Object(_) => b"__invoke".to_vec(),
        Value::Reference(r) => callable_function_name(&r.get()),
        _ => Vec::new(),
    }
}

fn opt_str(v: Option<&[u8]>) -> Value {
    v.map_or(Value::Null, |s| Value::Str(ZStr::from(s)))
}

/// Calls the user loader (`php_libxml_external_entity_loader`).
fn call_loader(vm: &mut Vm, callable: &Value, req: &EntityRequest) -> EntityLoaderResult {
    let mut ctx = ZArray::new();
    for (k, v) in req.context.entries() {
        ctx.make_mut().insert(k, opt_str(v));
    }
    let args = vec![
        opt_str(req.public_id.as_deref()),
        opt_str(req.system_id.as_deref()),
        Value::Array(ctx),
    ];
    let failed = || EntityLoaderResult::Failed {
        function_name: callable_function_name(callable),
    };
    let ret = match vm.call_value(callable, args) {
        Ok(v) => v.deref(),
        Err(t) => {
            bridge::set_pending(t);
            return failed();
        }
    };
    match ret {
        Value::Null => EntityLoaderResult::Null,
        Value::Str(s) => EntityLoaderResult::Path(s.as_bytes().to_vec()),
        Value::Resource(r) => match res::cell_of(&r) {
            Some(cell) if !r.is_closed() && !cell.is_closed() => {
                EntityLoaderResult::Stream(Box::new(ResourceReader { cell, _keep: r }))
            }
            _ => {
                let func = ACTIVE
                    .with(|s| s.borrow().last().cloned())
                    .unwrap_or_default();
                let msg = format!(
                    "{func}(): The user entity loader callback \"{}\" has returned a resource, but it is not a stream",
                    callable_display_name(callable)
                );
                let t = vm.throw("TypeError", &msg);
                bridge::set_pending(t);
                EntityLoaderResult::Null
            }
        },
        other => match vm.value_to_string(&other) {
            Ok(s) => EntityLoaderResult::Path(s.as_bytes().to_vec()),
            Err(t) => {
                bridge::set_pending(t);
                EntityLoaderResult::Null
            }
        },
    }
}

fn install_loader(callable: Option<Value>) {
    let loader: Option<Rc<xio::EntityLoader>> = callable.clone().map(|cb| {
        let f: Rc<xio::EntityLoader> = Rc::new(move |req: &EntityRequest| {
            bridge::with_vm(|vm| call_loader(vm, &cb, req)).unwrap_or(EntityLoaderResult::Null)
        });
        f
    });
    xio::set_external_entity_loader(loader);
    LOADER.with(|l| *l.borrow_mut() = callable);
}

// ---------------------------------------------------------------------
// PHP functions
// ---------------------------------------------------------------------

fn set_streams_context(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    if !matches!(v, Value::Resource(_)) {
        return Err(a.type_error(
            vm,
            1,
            &format!("must be of type resource, {} given", v.type_name()),
        ));
    }
    if let Some(ctx) = res::context_arg(vm, a, 0, true)? {
        xio::set_streams_context(Some(Rc::new(ctx)));
        CONTEXT.with(|c| *c.borrow_mut() = Some(v));
    }
    Ok(Value::Null)
}

fn use_internal_errors(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arg = a.opt(0).map(|v| matches!(v, Value::Bool(true)));
    Ok(Value::Bool(xerr::use_internal_errors(arg)))
}

fn get_last_error(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    match xerr::get_last_error() {
        Some(e) => new_error_object(vm, &e),
        None => Ok(Value::Bool(false)),
    }
}

fn get_errors(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut out = ZArray::new();
    for e in xerr::get_errors() {
        let o = new_error_object(vm, &e)?;
        let _ = out.make_mut().push(o);
    }
    Ok(Value::Array(out))
}

fn clear_errors_fn(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    xerr::clear_errors();
    Ok(Value::Null)
}

fn disable_entity_loader(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let disable = if a.has(0) { a.bool(0) } else { true };
    Ok(Value::Bool(xio::disable_entity_loader(disable)))
}

fn set_external_entity_loader(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    install_loader(a.opt(0));
    Ok(Value::Bool(true))
}

fn get_external_entity_loader(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(LOADER.with(|l| l.borrow().clone()).unwrap_or(Value::Null))
}

/// RINIT / RSHUTDOWN: resets the per-request libxml state.
fn reset_request(_vm: &mut Vm) {
    xerr::reset_request_state();
    install_loader(None);
    CONTEXT.with(|c| *c.borrow_mut() = None);
    ACTIVE.with(|s| s.borrow_mut().clear());
    ensure_hooks();
}

/// Registers the `libxml` module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    php_libxml::raw::ensure_init();
    ensure_hooks();
    for (name, value) in php_libxml::constants::all() {
        let v = match value {
            php_libxml::constants::ConstValue::Int(i) => Value::Int(i),
            php_libxml::constants::ConstValue::Str(s) => Value::Str(ZStr::from(s.as_str())),
        };
        m.constant(name, v);
    }
    ClassBuilder::new("LibXMLError")
        .prop("level", acc::PUBLIC, Some("int"), None)
        .prop("code", acc::PUBLIC, Some("int"), None)
        .prop("column", acc::PUBLIC, Some("int"), None)
        .prop("message", acc::PUBLIC, Some("string"), None)
        .prop("file", acc::PUBLIC, Some("string"), None)
        .prop("line", acc::PUBLIC, Some("int"), None)
        .register(m.vm());
    m.function(
        "libxml_set_streams_context($context): void",
        set_streams_context,
    )
    .function(
        "libxml_use_internal_errors(?bool $use_errors = null): bool",
        use_internal_errors,
    )
    .function("libxml_get_last_error(): LibXMLError|false", get_last_error)
    .function("libxml_get_errors(): array", get_errors)
    .function("libxml_clear_errors(): void", clear_errors_fn)
    .function(
        "libxml_disable_entity_loader(bool $disable = true): bool",
        disable_entity_loader,
    )
    .deprecated("8.0", "as external entity loading is disabled by default")
    .function(
        "libxml_set_external_entity_loader(?callable $resolver_function): true",
        set_external_entity_loader,
    )
    .function(
        "libxml_get_external_entity_loader(): ?callable",
        get_external_entity_loader,
    )
    .on_request_startup(reset_request)
    .on_request_shutdown(reset_request);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_names_of_callables() {
        assert_eq!(callable_function_name(&Value::from("foo")), b"foo");
        assert_eq!(callable_function_name(&Value::from("A::b")), b"b");
        let mut arr = ZArray::new();
        let _ = arr.make_mut().push(Value::from("A"));
        let _ = arr.make_mut().push(Value::from("handle"));
        assert_eq!(
            callable_function_name(&Value::Array(arr.clone())),
            b"handle"
        );
        assert_eq!(callable_display_name(&Value::Array(arr)), "A::handle");
    }

    #[test]
    fn opt_str_maps_none_to_null() {
        assert!(matches!(opt_str(None), Value::Null));
        assert!(matches!(opt_str(Some(b"x")), Value::Str(_)));
    }
}

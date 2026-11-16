// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! master's structured stream errors (`main/streams/stream_errors.c`):
//! the `StreamErrorCode`/`StreamErrorMode`/`StreamErrorStore` enums,
//! `StreamError`, `StreamException`, `stream_last_errors()` /
//! `stream_clear_errors()` and the `stream.error_mode` /
//! `stream.error_store` / `stream.error_handler` context options.
//!
//! Every library call made through [`crate::bridge::run`] is an
//! operation: the stream errors it raised (diagnostics with a code) are
//! reported when it ends, according to the context of the call.

use std::cell::RefCell;
use std::rc::Rc;

use php_runtime::{Obj, Value, ZArray};
use php_streams::context::ContextRef;
use php_streams::diag::{Diagnostic, Severity, StreamErrorCode};
use php_vm::class::Class;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge::{self, bytes};
use crate::res;

/// One collected stream error.
#[derive(Debug, Clone)]
struct Entry {
    code: StreamErrorCode,
    message: String,
    wrapper: String,
    severity: i64,
    terminating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Error,
    Exception,
    Silent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Store {
    None,
    NonTerminating,
    Terminating,
    All,
}

thread_local! {
    /// The context of the running operation (set by `context_arg` and
    /// stream operations).
    static OP_CONTEXT: RefCell<Option<ContextRef>> = const { RefCell::new(None) };
    /// Stored error groups, most recent last.
    static STORED: RefCell<Vec<Vec<Entry>>> = const { RefCell::new(Vec::new()) };
    static CLASSES: RefCell<Vec<Rc<Class>>> = const { RefCell::new(Vec::new()) };
}

/// The context of the running operation, without consuming it.
pub fn operation_context() -> Option<ContextRef> {
    OP_CONTEXT.with(|c| c.borrow().clone())
}

/// Records the context of the running operation.
pub fn set_operation_context(ctx: Option<ContextRef>) {
    OP_CONTEXT.with(|c| *c.borrow_mut() = ctx);
}

fn severity_level(s: Severity) -> i64 {
    match s {
        Severity::Warning => 2,
        Severity::Notice => 8,
        Severity::Deprecated => 8192,
        Severity::Error => 1,
    }
}

/// The `stream` option `name` of a context as a PHP value.
fn option(ctx: Option<&ContextRef>, name: &[u8]) -> Option<Value> {
    let cell = res::ctx_cell_of_lib(ctx?)?;
    let opts = cell.options.borrow();
    let stream = opts.get_str(b"stream")?.deref();
    stream.as_array()?.get_str(name).map(Value::deref)
}

/// The case name of an enum option of class `class`, or `Err` when the
/// value has another type.
fn enum_option(vm: &mut Vm, v: &Value, class: &str) -> Result<String, ()> {
    let Value::Object(o) = v else {
        return Err(());
    };
    if !o.class_name().eq_ignore_ascii_case(class.as_bytes()) {
        return Err(());
    }
    vm.get_property(o, "name")
        .ok()
        .and_then(|n| n.as_str().map(php_runtime::ZStr::to_string_lossy))
        .ok_or(())
}

fn auto_store(mode: Mode) -> Store {
    match mode {
        Mode::Error => Store::None,
        Mode::Exception => Store::NonTerminating,
        Mode::Silent => Store::All,
    }
}

/// Emits a diagnostic as a plain docref error.
fn emit(vm: &mut Vm, level: i64, text: &str) {
    let r = match level {
        2 => vm.php_warning(text),
        8 => vm.php_notice(text),
        8192 => vm.php_deprecated(text),
        _ => vm.raise(level, text),
    };
    if let Err(t) = r {
        bridge::set_pending(t);
    }
}

/// Reports the diagnostics of one operation.
#[allow(clippy::too_many_lines)] // one dispatch over the error modes
pub fn report(vm: &mut Vm, diags: Vec<Diagnostic>) {
    let ctx = OP_CONTEXT
        .with(|c| c.borrow_mut().take())
        .or_else(res::existing_default_context);
    if diags.len() > 1 && in_implicit_operation(vm) {
        // Without an explicit operation every error is its own implicit
        // one (`php_stream_wrapper_error_internal`): reported at once, in
        // order with the function's other warnings.
        for d in diags {
            report_in(vm, ctx.as_ref(), vec![d]);
        }
        return;
    }
    report_in(vm, ctx.as_ref(), diags);
}

/// ext/standard functions that do not begin a stream error operation
/// (filestat.c and friends).
const IMPLICIT_OPERATION_FUNCTIONS: &[&str] = &[
    "stat",
    "lstat",
    "file_exists",
    "is_file",
    "is_dir",
    "is_link",
    "is_readable",
    "is_writable",
    "is_writeable",
    "is_executable",
    "fileperms",
    "fileinode",
    "filesize",
    "fileowner",
    "filegroup",
    "fileatime",
    "filemtime",
    "filectime",
    "filetype",
    "touch",
    "chmod",
    "chown",
    "chgrp",
    "lchown",
    "lchgrp",
    "disk_free_space",
    "diskfreespace",
    "disk_total_space",
];

/// Whether the running function has no explicit stream error operation.
fn in_implicit_operation(vm: &Vm) -> bool {
    vm.frames
        .iter()
        .rev()
        .find_map(|f| f.builtin_name.as_ref())
        .is_some_and(|n| {
            let n = n.to_string_lossy().to_ascii_lowercase();
            IMPLICIT_OPERATION_FUNCTIONS.contains(&n.as_str())
        })
}

fn report_in(vm: &mut Vm, ctx: Option<&ContextRef>, diags: Vec<Diagnostic>) {
    let mut entries = Vec::new();
    for d in diags {
        // php_stream_display_wrapper_errors() stays silent once an
        // exception is pending.
        if d.code == StreamErrorCode::OpenFailed && bridge::has_pending() {
            continue;
        }
        if d.code == StreamErrorCode::None {
            emit(vm, severity_level(d.severity), &d.text());
        } else {
            entries.push(Entry {
                code: d.code,
                message: d.text(),
                wrapper: d.wrapper.clone(),
                severity: severity_level(d.severity),
                terminating: d.terminating,
            });
        }
    }
    if entries.is_empty() {
        return;
    }
    let mode = match option(ctx, b"error_mode") {
        None => Mode::Error,
        Some(v) => match enum_option(vm, &v, "StreamErrorMode").as_deref() {
            Ok("Exception") => Mode::Exception,
            Ok("Silent") => Mode::Silent,
            Ok(_) => Mode::Error,
            Err(()) => {
                let t = vm.throw(
                    "TypeError",
                    "stream context option 'error_mode' must be of type StreamErrorMode",
                );
                bridge::set_pending(t);
                Mode::Error
            }
        },
    };
    let store = match option(ctx, b"error_store") {
        None => auto_store(mode),
        Some(v) => match enum_option(vm, &v, "StreamErrorStore").as_deref() {
            Ok("None") => Store::None,
            Ok("NonTerminating") => Store::NonTerminating,
            Ok("Terminating") => Store::Terminating,
            Ok("All") => Store::All,
            Ok(_) => auto_store(mode),
            Err(()) => {
                let t = vm.throw(
                    "TypeError",
                    "stream context option 'error_store' must be of type StreamErrorStore",
                );
                bridge::set_pending(t);
                auto_store(mode)
            }
        },
    };
    let terminating = entries.iter().any(|e| e.terminating);
    match mode {
        Mode::Error => {
            for e in &entries {
                emit(vm, e.severity, &e.message);
            }
        }
        Mode::Exception if terminating => {
            if let Err(t) = throw_exception(vm, &entries) {
                bridge::set_pending(t);
            }
        }
        Mode::Exception | Mode::Silent => {}
    }
    // zend_call_function() does nothing while an exception is pending.
    let handler = option(ctx, b"error_handler").filter(|_| !bridge::has_pending());
    if let Some(handler) = handler {
        match errors_array(vm, &entries) {
            Ok(arr) => {
                if vm.is_callable(&handler) {
                    if let Err(t) = vm.call_value(&handler, vec![arr]) {
                        bridge::set_pending(t);
                    }
                } else if !bridge::has_pending() {
                    // `zend_is_callable()`'s error text says why.
                    let why = vm.callable_error_message(&handler);
                    let t = vm.throw(
                        "TypeError",
                        &format!("stream error handler must be a valid callback, {why}"),
                    );
                    bridge::set_pending(t);
                }
            }
            Err(t) => bridge::set_pending(t),
        }
    }
    let kept: Vec<Entry> = entries
        .into_iter()
        .filter(|e| match store {
            Store::None => false,
            Store::All => true,
            Store::NonTerminating => !e.terminating,
            Store::Terminating => e.terminating,
        })
        .collect();
    if !kept.is_empty() {
        STORED.with(|s| s.borrow_mut().push(kept));
    }
}

fn class(i: usize) -> Option<Rc<Class>> {
    CLASSES.with(|c| c.borrow().get(i).cloned())
}

fn code_case(vm: &mut Vm, code: StreamErrorCode) -> VmResult<Value> {
    let idx = StreamErrorCode::NAMES
        .iter()
        .position(|n| *n == code.name())
        .unwrap_or(1);
    let cases = vm.call_value(&bytes("StreamErrorCode::cases"), Vec::new())?;
    Ok(cases
        .as_array()
        .and_then(|a| a.get_int(i64::try_from(idx).unwrap_or(0)).cloned())
        .unwrap_or(Value::Null))
}

fn error_object(vm: &mut Vm, e: &Entry) -> VmResult<Value> {
    let Some(cls) = class(0) else {
        return Ok(Value::Null);
    };
    let o = vm.new_object(&cls)?;
    let code = code_case(vm, e.code)?;
    vm.set_property_raw(&o, "code", code);
    vm.set_property_raw(&o, "message", bytes(e.message.as_bytes()));
    vm.set_property_raw(&o, "wrapperName", bytes(e.wrapper.as_bytes()));
    vm.set_property_raw(&o, "severity", Value::Int(e.severity));
    vm.set_property_raw(&o, "terminating", Value::Bool(e.terminating));
    Ok(Value::Object(o))
}

fn errors_array(vm: &mut Vm, entries: &[Entry]) -> VmResult<Value> {
    let mut arr = ZArray::new();
    for e in entries {
        let v = error_object(vm, e)?;
        let _ = arr.make_mut().push(v);
    }
    Ok(Value::Array(arr))
}

fn code_id(code: StreamErrorCode) -> i64 {
    StreamErrorCode::NAMES
        .iter()
        .position(|n| *n == code.name())
        .map_or(0, |p| i64::try_from(p + 1).unwrap_or(0))
}

/// The `$errors` of a `StreamException` (read by `getErrors()`).
struct ExceptionErrors(Value);

fn throw_exception(vm: &mut Vm, entries: &[Entry]) -> VmResult<()> {
    let (Some(first), Some(cls)) = (entries.first(), class(1)) else {
        return Ok(());
    };
    let o: Obj = vm.new_object_with_args(
        &cls,
        vec![
            bytes(first.message.as_bytes()),
            Value::Int(code_id(first.code)),
        ],
    )?;
    let arr = errors_array(vm, entries)?;
    vm.set_property_raw(&o, "errors", arr.clone());
    php_vm::object::set_internal(&o, ExceptionErrors(arr));
    Err(php_vm::Throw::Exception(o))
}

/// Whether the options use the error handling keys (not allowed on the
/// default context).
pub fn has_error_settings(options: &ZArray) -> bool {
    options
        .get_str(b"stream")
        .map(Value::deref)
        .and_then(|v| {
            v.as_array().map(|a| {
                a.get_str(b"error_mode").is_some()
                    || a.get_str(b"error_store").is_some()
                    || a.get_str(b"error_handler").is_some()
            })
        })
        .unwrap_or(false)
}

fn stream_last_errors(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let last = STORED.with(|s| s.borrow().last().cloned());
    match last {
        Some(entries) => errors_array(vm, &entries),
        None => Ok(Value::Array(ZArray::new())),
    }
}

fn stream_clear_errors(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    STORED.with(|s| s.borrow_mut().clear());
    Ok(Value::Null)
}

fn get_errors(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(this) = a.this.clone() else {
        return Ok(Value::Array(ZArray::new()));
    };
    let _ = vm;
    Ok(
        php_vm::object::internal::<ExceptionErrors, _>(&this, |e| e.0.clone())
            .unwrap_or_else(|| Value::Array(ZArray::new())),
    )
}

/// Registers the enums, classes and functions.
pub fn register(m: &mut ModuleBuilder<'_>) {
    let mut code = ClassBuilder::new("StreamErrorCode").unit_enum();
    for n in StreamErrorCode::NAMES {
        code = code.case(n, None);
    }
    code.register(m.vm());
    let mut mode = ClassBuilder::new("StreamErrorMode").unit_enum();
    for n in ["Error", "Exception", "Silent"] {
        mode = mode.case(n, None);
    }
    mode.register(m.vm());
    let mut store = ClassBuilder::new("StreamErrorStore").unit_enum();
    for n in ["Auto", "None", "NonTerminating", "Terminating", "All"] {
        store = store.case(n, None);
    }
    store.register(m.vm());
    let ro = acc::PUBLIC | acc::READONLY;
    let error = ClassBuilder::new("StreamError")
        .flags(class_flags::FINAL)
        .prop("code", ro, Some("StreamErrorCode"), None)
        .prop("message", ro, Some("string"), None)
        .prop("wrapperName", ro, Some("string"), None)
        .prop("severity", ro, Some("int"), None)
        .prop("terminating", ro, Some("bool"), None)
        .register(m.vm());
    let exception = ClassBuilder::new("StreamException")
        .extends(m.vm(), "Exception")
        .prop(
            "errors",
            acc::PRIVATE,
            Some("array"),
            Some(Value::Array(ZArray::new())),
        )
        .method("getErrors(): array", acc::PUBLIC, get_errors)
        .register(m.vm());
    CLASSES.with(|c| *c.borrow_mut() = vec![error, exception]);
    m.function_static("stream_last_errors(): array", stream_last_errors)
        .function_static("stream_clear_errors(): void", stream_clear_errors);
}

#[cfg(test)]
mod tests {
    use php_vm::ext::Extension;
    use php_vm::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `php_stream_call_error_handler()`: the TypeError for a handler that
    /// is not callable carries `zend_is_callable()`'s reason.
    #[test]
    fn invalid_error_handler_names_the_reason() {
        let sink = Sink::default();
        let streams = crate::Streams;
        let exts: [&dyn Extension; 1] = [&streams];
        let mut vm = Vm::with_extensions(&VmOptions { ini: vec![] }, Box::new(sink.clone()), &exts);
        vm.run_script(
            br#"<?php
$ctx = stream_context_create(['stream' => [
    'error_mode' => StreamErrorMode::Silent,
    'error_handler' => 'no_such_handler',
]]);
try {
    fopen('/nonexistent-ws20/x', 'r', false, $ctx);
} catch (TypeError $e) {
    echo $e->getMessage(), "\n";
}
"#,
            "test.php",
        );
        drop(vm);
        let bytes = sink.0.borrow().clone();
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "stream error handler must be a valid callback, function \"no_such_handler\" not found or invalid function name\n"
        );
    }
}

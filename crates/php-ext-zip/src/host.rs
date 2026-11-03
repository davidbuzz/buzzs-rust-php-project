//! [`VmHost`]: the [`ZipHost`] implementation over the VM, and the
//! conversions between VM throws and `php-zip`'s runtime errors.
//!
//! Library calls run inside [`php_ext_streams::bridge::run`], which lends
//! the VM to code that has no direct access to it; the host reaches the VM
//! through [`bridge::with_vm`]. Warnings queued by the library are flushed
//! before any PHP code runs, so their order relative to callback output
//! matches PHP.

use std::cell::RefCell;

use php_ext_streams::bridge;
use php_runtime::{
    BinaryOp, CastTarget, ErrorLevel, Obj, RtResult, RuntimeContext, RuntimeError, Value,
};
use php_vm::ext::VmResult;
use php_vm::{Throw, Vm};
use php_zip::{RegexMatcher, ZipHost};

thread_local! {
    /// The `addPattern()` PCRE state (pattern cache). The `php-ext-pcre`
    /// state is private to that crate, so zip keeps its own.
    static PCRE: RefCell<php_pcre::Pcre> = RefCell::new(php_pcre::Pcre::new());
}

/// Converts a VM throw into a runtime error. Bailouts (`exit`, fatal
/// errors) are parked in [`Vm::pending_throw`] and come back out of
/// [`rt_throw`].
pub(crate) fn to_rt(vm: &mut Vm, t: Throw) -> RuntimeError {
    match t {
        Throw::Exception(o) => RuntimeError::Thrown(Value::Object(o)),
        other => {
            if vm.pending_throw.is_none() {
                vm.pending_throw = Some(other);
            }
            RuntimeError::Fatal(String::new())
        }
    }
}

/// Converts a runtime error into a VM throw, creating the exception object
/// for errors raised by `php-zip`.
pub(crate) fn rt_throw(vm: &mut Vm, e: RuntimeError) -> Throw {
    if let Some(t) = vm.pending_throw.take() {
        return t;
    }
    let class = e.class_name().unwrap_or("Error");
    match e {
        RuntimeError::Thrown(Value::Object(o)) => Throw::Exception(o),
        RuntimeError::Thrown(_) => Throw::Fatal,
        // A bailout whose cause was already reported.
        RuntimeError::Fatal(m) if m.is_empty() => Throw::Fatal,
        RuntimeError::Fatal(m) => vm.fatal(&m),
        RuntimeError::Custom { class, message } => vm.throw(&class.to_string_lossy(), &message),
        RuntimeError::Error(m)
        | RuntimeError::TypeError(m)
        | RuntimeError::ValueError(m)
        | RuntimeError::ArithmeticError(m)
        | RuntimeError::DivisionByZeroError(m)
        | RuntimeError::ArgumentCountError(m) => vm.throw(class, &m),
    }
}

/// The error returned when the VM is not reachable (cannot happen inside
/// [`run`]).
fn no_vm() -> RuntimeError {
    RuntimeError::Fatal(String::new())
}

/// The [`ZipHost`] handed to `php-zip`: every call goes to the VM lent by
/// the enclosing [`run`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct VmHost;

impl RuntimeContext for VmHost {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        bridge::with_vm(|vm| {
            bridge::flush_diags(vm);
            vm.diagnostic(level, message)
        })
        .unwrap_or(Ok(()))
    }

    fn precision(&self) -> i64 {
        bridge::with_vm(|vm| vm.precision()).unwrap_or(14)
    }

    fn serialize_precision(&self) -> i64 {
        bridge::with_vm(|vm| vm.serialize_precision()).unwrap_or(-1)
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        bridge::with_vm(|vm| vm.cast_object(obj, target)).unwrap_or(Ok(None))
    }

    fn do_operation(&mut self, op: BinaryOp, op1: &Value, op2: &Value) -> RtResult<Option<Value>> {
        bridge::with_vm(|vm| vm.do_operation(op, op1, op2)).unwrap_or(Ok(None))
    }

    fn compare_objects(&mut self, op1: &Value, op2: &Value) -> RtResult<i32> {
        bridge::with_vm(|vm| vm.compare_objects(op1, op2)).unwrap_or(Ok(1))
    }
}

impl ZipHost for VmHost {
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        bridge::with_vm(|vm| {
            // Warnings raised so far come before anything the callback does.
            bridge::flush_diags(vm);
            if let Some(t) = bridge::take_pending() {
                return Err(to_rt(vm, t));
            }
            let r = vm.call_value(callable, args.to_vec());
            // Destructors of values released by the callback run now, like
            // PHP's refcounting would.
            let r = r.and_then(|v| vm.run_released_destructors().map(|()| v));
            r.map_err(|t| to_rt(vm, t))
        })
        .unwrap_or_else(|| Err(no_vm()))
    }

    fn compile_regex(&mut self, regex: &[u8]) -> Option<RegexMatcher> {
        bridge::with_vm(|vm| compile_regex(vm, regex)).flatten()
    }
}

/// `pcre_get_compiled_regex()`: compiles through the PCRE cache, emitting
/// PCRE's warnings in order with the library's own diagnostics.
fn compile_regex(vm: &mut Vm, regex: &[u8]) -> Option<RegexMatcher> {
    let (result, warnings, cfg) = PCRE.with(|p| {
        let mut p = p.borrow_mut();
        p.set_backtrack_limit(vm.ini_long("pcre.backtrack_limit"));
        p.set_recursion_limit(vm.ini_long("pcre.recursion_limit"));
        p.set_jit(vm.ini_bool("pcre.jit"));
        let r = p.compile(regex);
        (r, p.take_warnings(), p.config())
    });
    for w in warnings {
        php_streams::diag::warning(w);
    }
    match result {
        Ok(re) => Some(Box::new(move |s: &[u8]| {
            matches!(php_pcre::match_one(&re, &cfg, s, false, 0, 0).result, Ok(n) if n > 0)
        })),
        Err(e) => {
            php_streams::diag::warning(e.message());
            None
        }
    }
}

/// Runs a `php-zip` call with a [`VmHost`]: diagnostics are emitted with
/// the running function's prefix afterwards, and errors (exceptions from
/// callbacks, bailouts, the library's own exceptions) become throws.
pub(crate) fn run<R>(
    vm: &mut Vm,
    f: impl FnOnce(&mut dyn ZipHost) -> RtResult<R>,
) -> VmResult<R> {
    let r = bridge::run(vm, || f(&mut VmHost));
    let r = match r {
        Ok(r) => r,
        Err(t) => {
            // A warning handler threw; a bailout parked meanwhile wins.
            return Err(vm.pending_throw.take().unwrap_or(t));
        }
    };
    match r {
        Ok(v) => match vm.pending_throw.take() {
            Some(t) => Err(t),
            None => Ok(v),
        },
        Err(e) => Err(rt_throw(vm, e)),
    }
}

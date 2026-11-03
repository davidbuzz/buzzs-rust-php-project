//! Shared plumbing: the per-object native state, the intl error model
//! (`intl_error_set()` and friends) and argument helpers.

use std::any::Any;
use std::cell::{Cell, RefCell};

use php_intl::{Error, ErrorState, IntlError, Scope, UErrorCode};
use php_runtime::{Obj, Value, ZStr};
use php_vm::ext::{Args, Throw, VmResult};
use php_vm::object;
use php_vm::vm::FrameKind;
use php_vm::Vm;

/// The native part of every intl object: its `intl_error` slot and the
/// class specific payload (`None` until constructed).
#[derive(Default)]
pub(crate) struct IntlObj {
    /// The object's last error.
    pub err: ErrorState,
    /// The class payload.
    pub data: Option<Box<dyn Any>>,
}

impl std::fmt::Debug for IntlObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntlObj")
            .field("err", &self.err)
            .field("constructed", &self.data.is_some())
            .finish()
    }
}

thread_local! {
    /// `INTL_G(g_error)`.
    static GLOBAL: RefCell<ErrorState> = RefCell::new(ErrorState::default());
    /// Set while a constructor runs: php-src forces
    /// `intl.use_exceptions = 1` and `intl.error_level = 0` there.
    static FORCE_THROW: Cell<u32> = const { Cell::new(0) };
}

/// While alive, intl errors throw `IntlException` and never warn (the
/// `__construct` convention of php-src master).
#[derive(Debug)]
pub(crate) struct CtorGuard(());

impl CtorGuard {
    /// Enters constructor mode.
    pub(crate) fn new() -> Self {
        FORCE_THROW.with(|c| c.set(c.get() + 1));
        CtorGuard(())
    }
}

impl Drop for CtorGuard {
    fn drop(&mut self) {
        FORCE_THROW.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Runs `f` in constructor mode (see [`CtorGuard`]).
pub(crate) fn in_ctor<R>(f: impl FnOnce() -> R) -> R {
    let _g = CtorGuard::new();
    f()
}

/// Looks up an intl class and instantiates it without its constructor.
pub(crate) fn instantiate(vm: &mut Vm, class: &str) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    vm.new_object(&cls)
}

/// The `create_object` hook shared by all intl classes.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, IntlObj::default());
    Ok(())
}

/// Runs `f` on the payload of `o` when it is constructed with type `T`.
pub(crate) fn with<T: 'static, R>(o: &Obj, f: impl FnOnce(&mut T) -> R) -> Option<R> {
    object::internal::<IntlObj, _>(o, |s| s.data.as_mut().and_then(|d| d.downcast_mut::<T>()).map(f)).flatten()
}

/// Whether `o` holds a payload of type `T`.
pub(crate) fn is_constructed<T: 'static>(o: &Obj) -> bool {
    with::<T, _>(o, |_| ()).is_some()
}

/// Stores the payload of `o`.
pub(crate) fn set_data<T: 'static>(o: &Obj, v: T) {
    let done = object::internal::<IntlObj, _>(o, |s| s.data = Some(Box::new(v) as Box<dyn Any>));
    if done.is_none() {
        // Objects created before the hook ran (should not happen).
        object::set_internal(o, IntlObj::default());
    }
}

/// Stores the payload of `o` (for values built in a closure).
pub(crate) fn set_boxed(o: &Obj, v: Box<dyn Any>) {
    if object::internal::<IntlObj, _>(o, |s| s.data = Some(v)).is_none() {
        object::set_internal(o, IntlObj::default());
    }
}

/// Removes the payload of `o`.
pub(crate) fn take_data(o: &Obj) -> Option<Box<dyn Any>> {
    object::internal::<IntlObj, _>(o, |s| s.data.take()).flatten()
}

/// The object's error slot.
pub(crate) fn obj_error(o: &Obj) -> ErrorState {
    object::internal::<IntlObj, _>(o, |s| s.err.clone()).unwrap_or_default()
}

/// Mutates the object's error slot.
pub(crate) fn with_obj_error<R>(o: &Obj, f: impl FnOnce(&mut ErrorState) -> R) -> Option<R> {
    object::internal::<IntlObj, _>(o, |s| f(&mut s.err))
}

/// The global error slot.
pub(crate) fn global_error() -> ErrorState {
    GLOBAL.with(|g| g.borrow().clone())
}

/// Mutates the global error slot.
pub(crate) fn with_global<R>(f: impl FnOnce(&mut ErrorState) -> R) -> R {
    GLOBAL.with(|g| f(&mut g.borrow_mut()))
}

/// `intl_error_reset(NULL)`.
pub(crate) fn reset_global() {
    with_global(ErrorState::reset);
}

/// `intl_error_reset(NULL)` + `intl_error_reset(obj)`.
pub(crate) fn reset_both(o: &Obj) {
    reset_global();
    with_obj_error(o, ErrorState::reset);
}

/// `get_active_function_or_method_name()`: the running builtin, else the
/// running user function, else `main`.
pub(crate) fn active_function(vm: &Vm) -> Option<String> {
    let top = vm.frames.last()?;
    if top.kind != FrameKind::Builtin {
        if top.kind != FrameKind::Function {
            return Some("main".to_owned());
        }
        let name = top.ufunc.as_ref().map(|u| u.name.to_string_lossy()).unwrap_or_default();
        return Some(match &top.scope {
            Some(c) if !name.contains("{closure") => format!("{}::{name}", object::display_name(&c.name)),
            _ => name,
        });
    }
    vm.frames
        .iter()
        .rev()
        .find(|f| f.kind == FrameKind::Builtin)
        .and_then(|f| {
            let n = f.builtin_name.clone()?;
            Some(match &f.scope {
                Some(c) if f.this.is_some() || f.called.is_some() => {
                    format!("{}::{}", object::display_name(&c.name), n.to_string_lossy())
                }
                _ => n.to_string_lossy(),
            })
        })
}

/// `intl.error_level` and `intl.use_exceptions`.
fn settings(vm: &Vm) -> (i64, bool) {
    if FORCE_THROW.with(Cell::get) > 0 {
        return (0, true);
    }
    (vm.ini_long("intl.error_level"), vm.ini_bool("intl.use_exceptions"))
}

/// `intl_error_set_custom_msg(NULL, msg)`: stores the prefixed message in
/// the global slot and emits the configured diagnostic / exception.
pub(crate) fn global_message(vm: &mut Vm, msg: &[u8]) -> VmResult<()> {
    let func = active_function(vm);
    let text = String::from_utf8_lossy(msg).into_owned();
    with_global(|g| g.set_message(func.as_deref(), &text));
    let (level, exc) = settings(vm);
    if level != 0 {
        vm.php_error_bytes(level, msg)?;
    }
    if exc {
        let prefixed = php_intl::error::prefix_message(func.as_deref(), &text);
        return Err(vm.throw("IntlException", &prefixed));
    }
    Ok(())
}

/// Records an [`IntlError`] like `intl_error_set()`/`intl_errors_set()`:
/// returns `Err` only when an exception must propagate.
pub(crate) fn record(vm: &mut Vm, o: Option<&Obj>, e: &IntlError) -> VmResult<()> {
    let func = active_function(vm);
    if e.always_throw {
        let text = String::from_utf8_lossy(e.message_bytes().unwrap_or_default()).into_owned();
        with_global(|g| g.code = e.code);
        if let Some(o) = o {
            with_obj_error(o, |s| s.code = e.code);
        }
        let prefixed = php_intl::error::prefix_message(func.as_deref(), &text);
        return Err(vm.throw("IntlException", &prefixed));
    }
    match e.scope {
        Scope::Both => {
            if let Some(o) = o {
                with_obj_error(o, |s| s.record(func.as_deref(), e));
            }
            with_global(|g| g.code = e.code);
        }
        Scope::ObjectCode => {
            if let Some(o) = o {
                with_obj_error(o, |s| s.record(func.as_deref(), e));
            }
        }
        Scope::Global => with_global(|g| g.code = e.code),
    }
    if let Some(m) = e.message_bytes() {
        let m = m.to_vec();
        global_message(vm, &m)?;
    }
    Ok(())
}

/// Turns a library error into PHP behaviour: engine exceptions are thrown
/// (`Err`), intl errors are recorded (`Ok`, the caller then returns its
/// failure value).  `shift` is added to argument numbers (1 for the
/// procedural forms taking the object first).
pub(crate) fn fail(vm: &mut Vm, a: &Args, o: Option<&Obj>, e: Error, shift: usize) -> VmResult<()> {
    match e {
        Error::Intl(ie) => record(vm, o, &ie),
        other => Err(throw_engine(vm, a, other, shift)),
    }
}

/// Builds the exception for a non-intl [`Error`].
pub(crate) fn throw_engine(vm: &mut Vm, a: &Args, e: Error, shift: usize) -> Throw {
    match e {
        Error::ArgumentValue { arg, message } => value_error(vm, a, arg as usize + shift, &message),
        Error::ArgumentType { arg, message } => type_error(vm, a, arg as usize + shift, &message),
        Error::Value(m) => vm.throw("ValueError", &m),
        Error::Thrown(m) => vm.throw("Error", &m),
        Error::ArgumentCount(m) => vm.throw("ArgumentCountError", &m),
        Error::IntlException(m) => vm.throw("IntlException", &m),
        Error::IntlExceptionInFunction(m) => {
            let func = active_function(vm);
            let msg = php_intl::error::prefix_message(func.as_deref(), &m);
            vm.throw("IntlException", &msg)
        }
        Error::Intl(ie) => {
            let msg = String::from_utf8_lossy(ie.message_bytes().unwrap_or_default()).into_owned();
            vm.throw("IntlException", &msg)
        }
    }
}

/// `fail()` and then return `ret`.
pub(crate) fn fail_with(vm: &mut Vm, a: &Args, o: Option<&Obj>, e: Error, shift: usize, ret: Value) -> VmResult<Value> {
    fail(vm, a, o, e, shift)?;
    Ok(ret)
}

/// An intl error recorded globally with a message (`intl_error_set(NULL, code, msg)`).
pub(crate) fn global_error_set(vm: &mut Vm, code: UErrorCode, msg: &str) -> VmResult<()> {
    record(vm, None, &IntlError::new(code, msg.to_owned()).with_scope(Scope::Global))
}

/// An intl error recorded in both slots (`intl_errors_set(&obj->err, code, msg)`).
pub(crate) fn errors_set(vm: &mut Vm, o: &Obj, code: UErrorCode, msg: &str) -> VmResult<()> {
    record(vm, Some(o), &IntlError::new(code, msg.to_owned()))
}

/// The call target: `$this` for methods, else the first argument (the
/// procedural form) together with the offset of the remaining arguments.
pub(crate) fn target(a: &Args) -> (Obj, usize) {
    match &a.this {
        Some(o) => (o.clone(), 0),
        None => match a.at(0) {
            Value::Object(o) => (o, 1),
            // ZPP guarantees an object for procedural aliases.
            _ => (a.this_obj(), 0),
        },
    }
}

/// `zend_throw_error(NULL, "Found unconstructed X")`.
pub(crate) fn unconstructed(vm: &mut Vm, what: &str) -> Throw {
    vm.throw("Error", &format!("Found unconstructed {what}"))
}

/// Shorthand: a string value from Rust text.
pub(crate) fn s(v: impl Into<ZStr>) -> Value {
    Value::Str(v.into())
}

/// `getErrorCode()` for any intl object.
pub(crate) fn get_error_code(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = target(a);
    Ok(Value::Int(i64::from(obj_error(&o).code.0)))
}

/// `getErrorMessage()` for any intl object.
pub(crate) fn get_error_message(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = target(a);
    Ok(s(obj_error(&o).get_message()))
}

/// The handler registered for stub entries without an implementation.
pub(crate) fn not_implemented(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = active_function(vm).unwrap_or_else(|| a.func_name());
    Err(vm.throw("Error", &format!("{name}() is not implemented in phprust yet")))
}

/// `ZEND_NUM_ARGS()`: the number of arguments actually passed.
pub(crate) fn num_args(a: &Args) -> usize {
    a.passed.iter().rposition(|&p| p).map_or(0, |i| i + 1)
}

/// `zend_zval_value_name()` for error messages.
pub(crate) fn type_name(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => object::class_of(o).name.to_string_lossy(),
        Value::Resource(_) => "resource".into(),
        Value::Reference(r) => type_name(&r.get()),
    }
}

fn arg_error(vm: &mut Vm, class: &str, a: &Args, n: usize, msg: &str) -> Throw {
    let fname = active_function(vm).unwrap_or_else(|| a.func_name());
    let pname = a
        .func
        .as_ref()
        .and_then(|f| f.params.get(n.wrapping_sub(1)).map(|p| p.name.to_string_lossy()))
        .unwrap_or_default();
    vm.throw(class, &format!("{fname}(): Argument #{n} (${pname}) {msg}"))
}

/// `zend_argument_value_error(n, msg)` (`Class::method` aware).
pub(crate) fn value_error(vm: &mut Vm, a: &Args, n: usize, msg: &str) -> Throw {
    arg_error(vm, "ValueError", a, n, msg)
}

/// `zend_argument_type_error(n, msg)` (`Class::method` aware).
pub(crate) fn type_error(vm: &mut Vm, a: &Args, n: usize, msg: &str) -> Throw {
    arg_error(vm, "TypeError", a, n, msg)
}

/// `zend_argument_error(class, n, msg)`.
pub(crate) fn argument_error(vm: &mut Vm, class: &str, a: &Args, n: usize, msg: &str) -> Throw {
    arg_error(vm, class, a, n, msg)
}

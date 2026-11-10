//! Glue between the VM's calling convention (`VmResult`, `Throw`, `Args`)
//! and php-stdarr's (`RtResult`, `RuntimeError`, closures).

use php_runtime::{ArrayKey, PhpRef, RtResult, Value, ZStr};
use php_stdarr::ArrayCallback;
use php_vm::ext::{Args, VmResult};
use php_vm::vm::PendingCall;
use php_vm::Vm;

/// Lifts a php-stdarr result into the VM.
pub fn rt<T>(vm: &mut Vm, r: RtResult<T>) -> VmResult<T> {
    r.map_err(|e| vm.throw_runtime_error(e))
}

/// A php-stdarr callback calling the PHP callable `cb`. The arguments are
/// moved into the call, so the callee sees PHP's refcounts.
///
/// The callable is resolved once, at the first call, and the cached call is
/// reused (`zend_fcall_info_cache`): `"C::m"` and array callables in the
/// calling scope, like `Z_PARAM_FUNC`'s `zend_is_callable_ex` (which
/// accepts `self`/`parent`/`static` and raises their deprecations), other
/// callables like [`Vm::call_value`]. A value that does not resolve is
/// passed to [`Vm::call_value`] on every call, which reports it.
pub fn callback(cb: Value) -> VmCallback {
    VmCallback {
        cb,
        state: CallbackState::Unresolved,
    }
}

/// The callback [`callback`] returns.
#[derive(Debug)]
pub struct VmCallback {
    /// The callable (released once a resolved call holds what it needs).
    cb: Value,
    /// The resolution.
    state: CallbackState,
}

#[derive(Debug)]
enum CallbackState {
    /// Not called yet.
    Unresolved,
    /// The cached call.
    Prepared(PendingCall),
    /// Not resolvable ahead: each call goes through [`Vm::call_value`].
    Dynamic,
}

impl VmCallback {
    /// Resolves the callable at the first call.
    fn resolve(&mut self, vm: &mut Vm) -> RtResult<()> {
        self.state = CallbackState::Dynamic;
        if needs_resolution(&self.cb) {
            match vm.resolve_callable(&self.cb) {
                Ok(Ok(call)) => self.state = CallbackState::Prepared(call),
                // Not callable that way: the direct path reports it.
                Ok(Err(_)) => {}
                Err(t) => return Err(vm.runtime_error_from(t)),
            }
        } else {
            match vm.prepare_call_value(&self.cb) {
                Ok(Some(call)) => {
                    // The cached call holds the closure or object; the
                    // callable value itself is no longer needed, so the
                    // callee sees the same refcounts as before caching.
                    self.cb = Value::Null;
                    self.state = CallbackState::Prepared(call);
                }
                Ok(None) => {}
                Err(t) => {
                    // Retried (and reported) by `call_value` next time.
                    self.state = CallbackState::Unresolved;
                    return Err(vm.runtime_error_from(t));
                }
            }
        }
        Ok(())
    }
}

impl ArrayCallback<Vm> for VmCallback {
    fn call(&mut self, vm: &mut Vm, mut args: Vec<Value>) -> RtResult<Value> {
        self.call_slice(vm, &mut args)
    }

    fn call_slice(&mut self, vm: &mut Vm, args: &mut [Value]) -> RtResult<Value> {
        if matches!(self.state, CallbackState::Unresolved) {
            self.resolve(vm)?;
        }
        let r = match &self.state {
            CallbackState::Prepared(call) => vm.call_prepared(call, args),
            _ => vm.call_value(&self.cb, args.iter_mut().map(std::mem::take).collect()),
        };
        r.map_err(|t| vm.runtime_error_from(t))
    }
}

/// Whether a callable names a method (`"C::m"`, `[class_or_object, m]`),
/// whose class part depends on the calling scope.
fn needs_resolution(cb: &Value) -> bool {
    match cb {
        Value::Str(s) => s.as_bytes().windows(2).any(|w| w == b"::"),
        Value::Array(_) => true,
        _ => false,
    }
}

/// The reference bound to by-reference argument `n` (a fresh reference
/// when the VM passed a plain value).
pub fn by_ref(a: &Args, n: usize) -> PhpRef {
    a.reference(n).unwrap_or_else(|| PhpRef::new(a.at(n)))
}

/// All arguments from position `from` on, dereferenced (the variadic
/// part; the VM already rejects named variadic arguments).
pub fn rest(a: &Args, from: usize) -> Vec<Value> {
    a.values.iter().skip(from).map(Value::deref).collect()
}

/// Checks a `callable` parameter like `Z_PARAM_FUNC`:
/// `func(): Argument #n ($name) must be a valid callback, <reason>`
/// (`or null` for nullable parameters, `$name` omitted for variadic
/// positions).
pub fn check_callable(
    vm: &mut Vm,
    a: &Args,
    n: usize,
    named: bool,
    nullable: bool,
    value: &Value,
) -> VmResult<()> {
    if nullable && value.is_null() {
        return Ok(());
    }
    // `zend_is_callable_ex` autoloads the class of a `"C::m"` / `["C", "m"]`
    // callable once; an exception thrown by an autoloader propagates.
    let reason = match callable_class(value) {
        Some(class) if vm.lookup_class(&class)?.is_none() => {
            format!("class \"{}\" not found", class.to_string_lossy())
        }
        _ if vm.is_callable(value) => return Ok(()),
        _ => vm.callable_error_message(value),
    };
    let label = if named {
        let pname = a
            .func
            .as_ref()
            .and_then(|f| f.params.get(n - 1).map(|p| p.name.to_string_lossy()))
            .unwrap_or_default();
        format!("Argument #{n} (${pname})")
    } else {
        format!("Argument #{n}")
    };
    let or_null = if nullable { " or null" } else { "" };
    let msg = format!(
        "{}(): {label} must be a valid callback{or_null}, {reason}",
        a.func_name()
    );
    Err(vm.throw("TypeError", &msg))
}

/// The class name a static-method callable refers to (not `self`,
/// `parent` or `static`, which are resolved against the calling scope).
fn callable_class(v: &Value) -> Option<ZStr> {
    let class = match v {
        Value::Str(s) => {
            let b = s.as_bytes();
            let p = b.windows(2).position(|w| w == b"::")?;
            ZStr::from(&b[..p])
        }
        Value::Array(a) if a.len() == 2 => match a.get(&ArrayKey::Int(0)).map(Value::deref) {
            Some(Value::Str(c)) => c,
            _ => return None,
        },
        _ => return None,
    };
    let lc = class.to_ascii_lowercase();
    let special = matches!(lc.as_bytes(), b"self" | b"parent" | b"static");
    (!special && !class.is_empty()).then_some(class)
}

/// Runs `f` with the request's legacy RNG (`mt_rand` state shared with
/// `mt_srand`), as php-stdarr's `range(min, max)` closure.
///
/// The state lives in `vm.ext.random` (owned by the VM's math builtins for
/// now; php-ext-random will take it over).
pub fn with_rng<R>(vm: &mut Vm, f: impl FnOnce(&mut php_stdarr::RangeFn<'_>) -> R) -> R {
    let mut g = vm.ext.random.take().unwrap_or_default();
    let r = {
        let mut range = |min: i64, max: i64| Ok(g.mt_rand_range_unchecked(min, max));
        f(&mut range)
    };
    vm.ext.random = Some(g);
    r
}

//! The SPL functions: `php-spl` implements them; the autoload registry is
//! mirrored into the VM's (`Vm::autoloaders`), which performs autoloading.

use crate::host::VmHost;
use php_runtime::{Value, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::vm::PendingCall;
use php_vm::{ClassLoader, Vm};
use std::sync::OnceLock;

/// Calls the `php-spl` implementation of the running function.
fn call(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(f) = a.func.clone() else {
        return Ok(Value::Null);
    };
    let name = f.name.to_string_lossy().to_ascii_lowercase();
    if let Some(msg) = php_spl::classes::function_deprecation_message(&name) {
        vm.raise(8192, &msg)?;
    }
    let n = a.passed.iter().rposition(|&p| p).map_or(0, |i| i + 1);
    let mut args: Vec<Value> = a.values.iter().take(n).cloned().collect();
    let mut fcc = None;
    let mut resolved = false;
    if matches!(
        name.as_str(),
        "spl_autoload_register" | "spl_autoload_unregister"
    ) {
        if let Some(cb) = args.first_mut() {
            if !cb.is_null() {
                // `Z_PARAM_FUNC`: the callable is resolved (and its
                // deprecations raised) in the registering scope.
                fcc = vm.resolve_callable(&*cb)?.ok();
                *cb = scope_keyword_callable(vm, &static_method_callable(cb));
                resolved = true;
            }
        }
    }
    let r = {
        let _entry = crate::files::enter();
        // `php-spl` validates the (rewritten) callable again: without
        // raising the deprecations a second time.
        let mut host = if resolved {
            VmHost::callable_checked(vm)
        } else {
            VmHost::new(vm)
        };
        php_spl::call_function(&mut host, &name, &args)
    };
    let Some(r) = r else {
        return Err(vm.throw("Error", &format!("Call to undefined function {name}()")));
    };
    if name.starts_with("spl_autoload_") {
        let registered = if name == "spl_autoload_register" {
            fcc
        } else {
            None
        };
        sync_autoloaders(vm, registered.as_ref());
    }
    crate::iter::finish(vm, r)
}

/// Mirrors the `php-spl` registry into the VM's loader table. Loaders
/// that stay registered keep the call they were resolved to; a newly
/// registered one gets `registered`.
fn sync_autoloaders(vm: &mut Vm, registered: Option<&PendingCall>) {
    let mut old: Vec<Option<ClassLoader>> = std::mem::take(&mut vm.autoloaders)
        .into_iter()
        .map(Some)
        .collect();
    vm.autoloaders = php_spl::functions::autoloaders()
        .into_iter()
        .map(|callable| {
            let loader = ClassLoader::unresolved(callable);
            let kept = old
                .iter()
                .position(|o| o.as_ref().is_some_and(|o| o.is_same(&loader)));
            let fcc = match kept {
                Some(i) => old[i].take().and_then(|o| o.fcc),
                None => registered.cloned(),
            };
            ClassLoader { fcc, ..loader }
        })
        .collect();
}

/// `zend_get_callable_zval_from_fcc` for `['self' | 'parent' | 'static',
/// 'm']` and `'self::m'`: the registry stores and reports the class the
/// keyword stands for in the registering scope, so the same method name
/// registered from two classes is two loaders.
fn scope_keyword_callable(vm: &Vm, cb: &Value) -> Value {
    match cb.deref() {
        Value::Array(arr) if arr.len() == 2 => {
            let (Some(Value::Str(keyword)), Some(method)) = (
                arr.get_int(0).map(Value::deref),
                arr.get_int(1).map(Value::deref),
            ) else {
                return cb.clone();
            };
            match vm.callable_keyword_class(keyword.as_bytes()) {
                Some(cls) => Value::Array(php_runtime::ZArray::from_values(vec![
                    Value::Str(cls.name.clone()),
                    method,
                ])),
                None => cb.clone(),
            }
        }
        Value::Str(s) => {
            let b = s.as_bytes();
            let Some(p) = b.windows(2).position(|w| w == b"::") else {
                return cb.clone();
            };
            match vm.callable_keyword_class(&b[..p]) {
                Some(cls) => {
                    let mut out = cls.name.as_bytes().to_vec();
                    out.extend_from_slice(&b[p..]);
                    Value::Str(ZStr::from(out))
                }
                None => cb.clone(),
            }
        }
        _ => cb.clone(),
    }
}

/// `[$object, 'staticMethod']` resolves to a function call without an
/// object (`zend_is_callable_ex` leaves `fcc.object` NULL), so the
/// autoload registry stores and compares it as `['Class', 'staticMethod']`.
fn static_method_callable(cb: &Value) -> Value {
    let Value::Array(arr) = cb.deref() else {
        return cb.clone();
    };
    if arr.len() != 2 {
        return cb.clone();
    }
    let (Some(Value::Object(o)), Some(Value::Str(m))) = (
        arr.get_int(0).map(Value::deref),
        arr.get_int(1).map(Value::deref),
    ) else {
        return cb.clone();
    };
    let cls = php_vm::object::class_of(&o).clone();
    let is_static = cls
        .find_method(m.to_ascii_lowercase().as_bytes())
        .is_some_and(|meth| meth.flags & php_vm::func::flags::STATIC != 0);
    if is_static {
        Value::Array(php_runtime::ZArray::from_values(vec![
            Value::Str(cls.name.clone()),
            Value::Str(m),
        ]))
    } else {
        cb.clone()
    }
}

/// Resolves an include path for `spl_autoload()` (`zend_resolve_path`):
/// relative names are looked up along `include_path`, then in the
/// directory of the executing script.
#[must_use]
pub fn resolve_include(vm: &Vm, file: &str) -> Option<String> {
    let p = std::path::Path::new(file);
    let exists = |p: &std::path::Path| p.is_file();
    if p.is_absolute() || file.starts_with("./") || file.starts_with("../") {
        return exists(p).then(|| file.to_string());
    }
    let ip = vm.ini_string("include_path").map_or_else(
        || ".".to_string(),
        |v| String::from_utf8_lossy(&v).into_owned(),
    );
    for dir in ip.split(':').filter(|d| !d.is_empty()) {
        let cand = std::path::Path::new(dir).join(file);
        if exists(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    let (script, _) = vm.current_location();
    let script = script.to_string_lossy();
    if let Some(dir) = std::path::Path::new(&script).parent() {
        let cand = dir.join(file);
        if exists(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

/// Registers the SPL functions (replacing the VM's simplified versions).
pub fn register(m: &mut ModuleBuilder<'_>) {
    static SIGS: OnceLock<Vec<String>> = OnceLock::new();
    let sigs = SIGS.get_or_init(|| {
        php_spl::classes::function_table()
            .iter()
            // The VM's autoload trigger and object id are authoritative.
            .filter(|f| !matches!(f.name, "spl_autoload_call" | "spl_object_id"))
            .map(crate::dispatch::function_signature)
            .collect()
    });
    for sig in sigs {
        m.function(sig, call);
    }
}

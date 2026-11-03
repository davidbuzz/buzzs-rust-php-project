//! The SPL functions: `php-spl` implements them; the autoload registry is
//! mirrored into the VM's (`Vm::autoloaders`), which performs autoloading.

use crate::host::VmHost;
use php_runtime::Value;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
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
    if matches!(
        name.as_str(),
        "spl_autoload_register" | "spl_autoload_unregister"
    ) {
        if let Some(cb) = args.first_mut() {
            *cb = static_method_callable(cb);
        }
    }
    let r = {
        let mut host = VmHost::new(vm);
        php_spl::call_function(&mut host, &name, &args)
    };
    let Some(r) = r else {
        return Err(vm.throw("Error", &format!("Call to undefined function {name}()")));
    };
    if name.starts_with("spl_autoload_") {
        vm.autoloaders = php_spl::functions::autoloaders();
    }
    crate::iter::finish(vm, r)
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

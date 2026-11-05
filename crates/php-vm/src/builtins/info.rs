//! Module introspection: `get_extension_funcs()` and the categorized
//! `get_defined_constants(true)` (`Zend/zend_builtin_functions.c`).
//!
//! Functions and constants registered through the extension API are
//! attributed to their module with [`note_function`] / [`note_constant`];
//! the ones registered by the VM's own builtins are split between `Core`
//! (the `zend_builtin_functions` set and the engine constants) and
//! `standard`.

// `ZStr` caches its hash in a `Cell`; `Hash`/`Eq` only use the bytes.
#![allow(clippy::mutable_key_type)]

use super::reg;
use super::util::{b, s};
use crate::func::{Args, Func};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// `Zend/zend_builtin_functions.stub.php`.
const CORE_FUNCTIONS: &[&str] = &[
    "zend_version",
    "func_num_args",
    "func_get_arg",
    "func_get_args",
    "strlen",
    "strcmp",
    "strncmp",
    "strcasecmp",
    "strncasecmp",
    "error_reporting",
    "define",
    "defined",
    "get_class",
    "get_called_class",
    "get_parent_class",
    "is_subclass_of",
    "is_a",
    "get_class_vars",
    "get_object_vars",
    "get_mangled_object_vars",
    "get_class_methods",
    "method_exists",
    "property_exists",
    "class_exists",
    "interface_exists",
    "trait_exists",
    "enum_exists",
    "function_exists",
    "class_alias",
    "get_included_files",
    "get_required_files",
    "trigger_error",
    "user_error",
    "set_error_handler",
    "restore_error_handler",
    "get_error_handler",
    "set_exception_handler",
    "restore_exception_handler",
    "get_exception_handler",
    "get_declared_classes",
    "get_declared_traits",
    "get_declared_interfaces",
    "get_defined_functions",
    "get_defined_vars",
    "get_resource_type",
    "get_resource_id",
    "get_resources",
    "get_loaded_extensions",
    "get_defined_constants",
    "debug_backtrace",
    "debug_print_backtrace",
    "extension_loaded",
    "get_extension_funcs",
    "zend_thread_id",
    "gc_mem_caches",
    "gc_collect_cycles",
    "gc_enabled",
    "gc_enable",
    "gc_disable",
    "gc_status",
    "clone",
    "exit",
    "die",
];

#[derive(Default)]
struct Registry {
    /// Lowercase function name → module, in registration order.
    functions: Vec<(ZStr, &'static str)>,
    #[allow(dead_code)]
    function_index: HashMap<ZStr, usize>,
    /// Constant name → module, in registration order.
    constants: Vec<(ZStr, &'static str)>,
    #[allow(dead_code)]
    constant_index: HashMap<ZStr, usize>,
    /// The VM's own builtin functions (snapshot at registration).
    builtin_functions: HashSet<ZStr>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Records that `module` registered the function `lc_name` (called by the
/// extension API).
#[allow(dead_code)] // for the extension API (module attribution)
pub fn note_function(lc_name: &ZStr, module: &'static str) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if let Some(&i) = r.function_index.get(lc_name) {
            r.functions[i].1 = module;
        } else {
            let i = r.functions.len();
            r.functions.push((lc_name.clone(), module));
            r.function_index.insert(lc_name.clone(), i);
        }
    });
}

/// Records that `module` registered the constant `name`.
#[allow(dead_code)] // for the extension API (module attribution)
pub fn note_constant(name: &ZStr, module: &'static str) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if let Some(&i) = r.constant_index.get(name) {
            r.constants[i].1 = module;
        } else {
            let i = r.constants.len();
            r.constants.push((name.clone(), module));
            r.constant_index.insert(name.clone(), i);
        }
    });
}

/// Whether an unattributed engine constant belongs to `Core`.
fn is_core_constant(name: &[u8]) -> bool {
    const PREFIXES: &[&[u8]] = &[
        b"E_",
        b"PHP_",
        b"ZEND_",
        b"DEBUG_BACKTRACE_",
        b"UPLOAD_ERR_",
        b"PEAR_",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
        || matches!(
            name,
            b"TRUE"
                | b"FALSE"
                | b"NULL"
                | b"DEFAULT_INCLUDE_PATH"
                | b"STDIN"
                | b"STDOUT"
                | b"STDERR"
                | b"__COMPILER_HALT_OFFSET__"
        )
}

/// The module name as `get_loaded_extensions()` spells it.
fn canonical_module(vm: &Vm, lc: &str) -> Option<String> {
    crate::LOADED_EXTENSIONS
        .iter()
        .map(|e| (*e).to_string())
        .chain(vm.loaded_extensions.iter().cloned())
        .find(|e| e.eq_ignore_ascii_case(lc))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn get_extension_funcs(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = s(a, 0).to_string_lossy();
    // "zend*" names the Core module.
    let module = if name.len() >= 4 && name[..4].eq_ignore_ascii_case("zend") {
        "Core".to_string()
    } else {
        match canonical_module(vm, &name) {
            Some(m) => m,
            None => return Ok(Value::Bool(false)),
        }
    };
    let names: Vec<ZStr> = REGISTRY.with(|r| {
        let r = r.borrow();
        let noted: HashSet<&ZStr> = r.functions.iter().map(|(n, _)| n).collect();
        let mut out: Vec<ZStr> = r
            .functions
            .iter()
            .filter(|(_, m)| m.eq_ignore_ascii_case(&module))
            .map(|(n, _)| n.clone())
            .collect();
        let core = module == "Core";
        if core || module == "standard" {
            let mut builtin: Vec<&ZStr> = r
                .builtin_functions
                .iter()
                .filter(|n| !noted.contains(n))
                .filter(|n| CORE_FUNCTIONS.contains(&n.to_string_lossy().as_str()) == core)
                .collect();
            builtin.sort();
            out.extend(builtin.into_iter().cloned());
        }
        out
    });
    let out: Vec<Value> = names
        .into_iter()
        .filter_map(|n| match vm.functions.get(&n) {
            Some(Func::Builtin(b)) => Some(Value::Str(b.name.clone())),
            _ => None,
        })
        .collect();
    if out.is_empty() {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn get_defined_constants(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let categorize = b(a, 0);
    let user: HashSet<&ZStr> = vm.user_constants.iter().collect();
    // Internal constants: attributed ones in registration order, then the
    // rest (engine builtins) by name.
    let mut internal: Vec<(ZStr, String)> = REGISTRY.with(|r| {
        r.borrow()
            .constants
            .iter()
            .filter(|(n, _)| vm.constants.contains_key(n) && !user.contains(n))
            .map(|(n, m)| (n.clone(), (*m).to_string()))
            .collect()
    });
    let noted: HashSet<ZStr> = internal.iter().map(|(n, _)| n.clone()).collect();
    let mut rest: Vec<&ZStr> = vm
        .constants
        .keys()
        .filter(|n| !noted.contains(*n) && !user.contains(n))
        .collect();
    rest.sort();
    let mut engine: Vec<(ZStr, String)> = rest
        .into_iter()
        .map(|n| {
            let m = if is_core_constant(n.as_bytes()) {
                "Core"
            } else {
                "standard"
            };
            (n.clone(), m.to_string())
        })
        .collect();
    // Core first, like the module registry.
    engine.sort_by_key(|(_, m)| m != "Core");
    let core_count = engine.iter().take_while(|(_, m)| m == "Core").count();
    let mut all: Vec<(ZStr, String)> = engine.drain(..core_count).collect();
    all.append(&mut engine);
    all.append(&mut internal);
    for n in &vm.user_constants {
        all.push((n.clone(), "user".to_string()));
    }
    let value_of = |n: &ZStr| vm.constants.get(n).cloned();
    if !categorize {
        let mut t = HashTable::new();
        for (n, _) in &all {
            if let Some(v) = value_of(n) {
                t.insert(ArrayKey::Str(n.clone()), v);
            }
        }
        return Ok(Value::Array(ZArray::from_table(t)));
    }
    let mut groups: Vec<(String, HashTable)> = Vec::new();
    for (n, m) in &all {
        let Some(v) = value_of(n) else {
            continue;
        };
        let idx = if let Some(i) = groups.iter().position(|(g, _)| g == m) {
            i
        } else {
            groups.push((m.clone(), HashTable::new()));
            groups.len() - 1
        };
        groups[idx].1.insert(ArrayKey::Str(n.clone()), v);
    }
    let mut t = HashTable::new();
    for (g, h) in groups {
        t.insert(
            ArrayKey::Str(ZStr::from(g)),
            Value::Array(ZArray::from_table(h)),
        );
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

/// Registers the introspection functions; must run after the other builtin
/// groups (it snapshots their functions as `Core`/`standard`).
pub(crate) fn register(vm: &mut Vm) {
    reg(
        vm,
        "get_extension_funcs(string $extension): array|false",
        get_extension_funcs,
    );
    reg(
        vm,
        "get_defined_constants(bool $categorize = false): array",
        get_defined_constants,
    );
    let names: HashSet<ZStr> = vm
        .functions
        .iter()
        .filter(|(_, f)| matches!(f, Func::Builtin(_)))
        .map(|(k, _)| k.clone())
        .collect();
    REGISTRY.with(|r| r.borrow_mut().builtin_functions = names);
}

#[cfg(test)]
mod tests {
    use super::is_core_constant;

    #[test]
    fn core_constants() {
        assert!(is_core_constant(b"E_ALL"));
        assert!(is_core_constant(b"PHP_VERSION"));
        assert!(!is_core_constant(b"M_PI"));
    }
}

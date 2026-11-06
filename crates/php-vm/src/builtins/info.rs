//! Module introspection: `get_extension_funcs()` and the categorized
//! `get_defined_constants(true)` (`Zend/zend_builtin_functions.c`), and
//! the registry that attributes internal functions, classes and constants
//! to their extensions (php-src keeps the `module_number` in each
//! `zend_function`/`zend_constant`).
//!
//! The registry answers two questions that php-src answers from the same
//! field but that have different rules here:
//!
//! * which module registered a function or constant through the extension
//!   API, in registration order, a later registration of the same name
//!   re-attributing it (`get_extension_funcs()`,
//!   `get_defined_constants(true)`; functions and constants registered by
//!   the VM's own builtins are split between `Core`, the
//!   `zend_builtin_functions` set and the engine constants, and `standard`);
//! * which extension's `register` inserted a function, class or constant
//!   as a *new* key, through any path (`ReflectionExtension`,
//!   `ReflectionFunction::getExtensionName()`, ...).
//!
//! One entry per symbol, keyed by the `ZStr` the VM's table uses, records
//! both ([`note_function`], [`note_constant`], [`note_new_function`],
//! [`note_new_constant`], [`note_new_class`]).

// `ZStr` caches its hash in a `Cell`; `Hash`/`Eq` only use the bytes.
#![allow(clippy::mutable_key_type)]

use super::fasthash::FastMap;
use super::reg;
use super::util::{b, s};
use crate::func::{Args, Func};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashSet;

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

/// A symbol's entry in an [`Attributed`] table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    /// Its place in [`Attributed::list`], once the extension API has
    /// registered it (`None` while only an engine insert site saw it: an
    /// alias, or a constant the VM defined inside an extension's
    /// `register`).
    pos: Option<usize>,
    /// The extension whose `register` inserted it as a new key
    /// (Reflection's attribution).
    origin: Option<&'static str>,
}

/// The functions or the constants: a registration-order list of
/// `(name, module)` for the names the extension API registered, and an
/// index over every noted name with its Reflection attribution.
#[derive(Default)]
struct Attributed {
    list: Vec<(ZStr, &'static str)>,
    index: FastMap<ZStr, Slot>,
}

impl Attributed {
    fn reserve(&mut self, n: usize) {
        self.list.reserve(n);
        self.index.reserve(n);
    }

    /// Records that `module` registered `name` through the extension API:
    /// a name registered again keeps its place and takes the new module.
    /// `origin` (when given) is the extension whose `register` inserted
    /// the name as a new key, which replaces the earlier attribution like
    /// the insert of a fresh key did.
    fn note_module(&mut self, name: &ZStr, module: &'static str, origin: Option<&'static str>) {
        match self.index.entry(name.clone()) {
            Entry::Occupied(mut e) => {
                let slot = e.get_mut();
                match slot.pos {
                    Some(p) => self.list[p].1 = module,
                    None => {
                        slot.pos = Some(self.list.len());
                        self.list.push((name.clone(), module));
                    }
                }
                if origin.is_some() {
                    slot.origin = origin;
                }
            }
            Entry::Vacant(e) => {
                e.insert(Slot {
                    pos: Some(self.list.len()),
                    origin,
                });
                self.list.push((name.clone(), module));
            }
        }
    }

    /// Records that `origin`'s `register` inserted `name` as a new key
    /// outside the extension API.
    fn note_origin(&mut self, name: &ZStr, origin: &'static str) {
        match self.index.entry(name.clone()) {
            Entry::Occupied(mut e) => e.get_mut().origin = Some(origin),
            Entry::Vacant(e) => {
                e.insert(Slot {
                    pos: None,
                    origin: Some(origin),
                });
            }
        }
    }

    /// The extension whose `register` inserted `name` as a new key.
    fn origin(&self, name: &[u8]) -> Option<&'static str> {
        self.index.get(name).and_then(|slot| slot.origin)
    }
}

#[derive(Default)]
struct Registry {
    /// Lower-case function names.
    functions: Attributed,
    /// Constant names.
    constants: Attributed,
    /// Lower-case class name → the extension whose `register` inserted it.
    classes: FastMap<ZStr, &'static str>,
    /// The VM's own builtin functions (snapshot at registration).
    builtin_functions: HashSet<ZStr>,
    /// The extension whose `register` is running.
    current: Option<&'static str>,
}

impl Registry {
    /// Room for what all bundled extensions register (about 1 950
    /// functions, 380 classes and 2 800 constants), so the tables do not
    /// rehash during start-up.
    fn reserve(&mut self) {
        if self.functions.list.capacity() == 0 {
            self.functions.reserve(2048);
            self.constants.reserve(4096);
            self.classes.reserve(512);
        }
    }
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Opens (`Some(name)`) or closes (`None`) the window in which every *new*
/// function, class and constant is attributed to extension `name` for
/// Reflection (the insert sites report new keys).
///
/// This replaces comparing full snapshots of the three tables before and
/// after each extension, which was O(extensions × symbols) and 70 % of
/// debug-build startup.
pub(crate) fn set_current_module(name: Option<&'static str>) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.reserve();
        r.current = name;
    });
}

/// Leaks the registry's entries, for a process that exits next
/// ([`Vm::discard`]): they name the internal functions, classes and
/// constants whose tables `discard` leaks too, and releasing the five
/// thousand names one by one was most of what the thread-local destructors
/// cost at exit. Readers see an empty registry afterwards.
pub(crate) fn forget_registry() {
    // `try_with`: the thread's locals may already be being destroyed.
    let _ = REGISTRY.try_with(|r| {
        if let Ok(mut r) = r.try_borrow_mut() {
            std::mem::forget(std::mem::take(&mut *r));
        }
    });
}

/// Records that `module` registered the function `lc_name` through the
/// extension API; `is_new` tells whether the name was not in the function
/// table before (Reflection then attributes it to the extension being
/// registered).
pub(crate) fn note_function(lc_name: &ZStr, module: &'static str, is_new: bool) {
    REGISTRY.with(|r| {
        let r = &mut *r.borrow_mut();
        r.reserve();
        let origin = if is_new { r.current } else { None };
        r.functions.note_module(lc_name, module, origin);
    });
}

/// Records a function the engine inserted as a new key outside the
/// extension API (a VM builtin, an alias): Reflection attributes it to the
/// extension being registered, if any.
pub(crate) fn note_new_function(lc_name: &ZStr) {
    REGISTRY.with(|r| {
        let r = &mut *r.borrow_mut();
        if let Some(current) = r.current {
            r.functions.note_origin(lc_name, current);
        }
    });
}

/// Records that `module` registered the constant `name` through the
/// extension API; `is_new` as for [`note_function`].
pub(crate) fn note_constant(name: &ZStr, module: &'static str, is_new: bool) {
    REGISTRY.with(|r| {
        let r = &mut *r.borrow_mut();
        r.reserve();
        let origin = if is_new { r.current } else { None };
        r.constants.note_module(name, module, origin);
    });
}

/// Records a constant the engine defined as a new key outside the
/// extension API: Reflection attributes it to the extension being
/// registered, if any.
pub(crate) fn note_new_constant(name: &ZStr) {
    REGISTRY.with(|r| {
        let r = &mut *r.borrow_mut();
        if let Some(current) = r.current {
            r.constants.note_origin(name, current);
        }
    });
}

/// Records a class inserted as a new key (lower-case name): Reflection
/// attributes it to the extension being registered, if any.
pub(crate) fn note_new_class(lc_name: &ZStr) {
    REGISTRY.with(|r| {
        let r = &mut *r.borrow_mut();
        if let Some(current) = r.current {
            r.reserve();
            r.classes.insert(lc_name.clone(), current);
        }
    });
}

/// The extension whose `register` inserted the function `lc_name`.
pub(crate) fn function_origin(lc_name: &[u8]) -> Option<&'static str> {
    REGISTRY.with(|r| r.borrow().functions.origin(lc_name))
}

/// The extension whose `register` inserted the class `lc_name`.
pub(crate) fn class_origin(lc_name: &[u8]) -> Option<&'static str> {
    REGISTRY.with(|r| r.borrow().classes.get(lc_name).copied())
}

/// The extension whose `register` inserted the constant `name`.
pub(crate) fn constant_origin(name: &[u8]) -> Option<&'static str> {
    REGISTRY.with(|r| r.borrow().constants.origin(name))
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
        let noted: HashSet<&ZStr> = r.functions.list.iter().map(|(n, _)| n).collect();
        let mut out: Vec<ZStr> = r
            .functions
            .list
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
            .list
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
    // `zend_constant.name`, not the hash key (GH-11423).
    let name_of = |n: &ZStr| vm.const_names.get(n).unwrap_or(n).clone();
    if !categorize {
        let mut t = HashTable::new();
        for (n, _) in &all {
            if let Some(v) = value_of(n) {
                t.insert(ArrayKey::Str(name_of(n)), v);
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
        groups[idx].1.insert(ArrayKey::Str(name_of(n)), v);
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
    use super::{is_core_constant, Attributed, Slot};
    use php_runtime::ZStr;

    #[test]
    fn core_constants() {
        assert!(is_core_constant(b"E_ALL"));
        assert!(is_core_constant(b"PHP_VERSION"));
        assert!(!is_core_constant(b"M_PI"));
    }

    fn listed(t: &Attributed) -> Vec<(Vec<u8>, &'static str)> {
        t.list
            .iter()
            .map(|(n, m)| (n.as_bytes().to_vec(), *m))
            .collect()
    }

    #[test]
    fn a_name_noted_again_keeps_its_place_and_takes_the_new_module() {
        let mut t = Attributed::default();
        for (name, module) in [("a", "one"), ("b", "one"), ("a", "two"), ("c", "three")] {
            t.note_module(&ZStr::from(name), module, None);
        }
        assert_eq!(
            listed(&t),
            [
                (b"a".to_vec(), "two"),
                (b"b".to_vec(), "one"),
                (b"c".to_vec(), "three")
            ]
        );
        assert_eq!(t.index.len(), 3);
        assert_eq!(t.index[b"c".as_slice()].pos, Some(2));
        assert_eq!(t.origin(b"a"), None);
    }

    #[test]
    fn an_engine_insert_sets_the_origin_without_a_place_in_the_module_list() {
        let mut t = Attributed::default();
        t.note_origin(&ZStr::from("alias_fn"), "ext");
        assert!(t.list.is_empty());
        assert_eq!(t.origin(b"alias_fn"), Some("ext"));
        assert_eq!(
            t.index[b"alias_fn".as_slice()],
            Slot {
                pos: None,
                origin: Some("ext")
            }
        );
        // Registered through the API later: it takes its place at the end,
        // as the module list of the old registry did.
        t.note_module(&ZStr::from("first"), "ext", Some("ext"));
        t.note_module(&ZStr::from("alias_fn"), "ext", None);
        assert_eq!(
            listed(&t),
            [(b"first".to_vec(), "ext"), (b"alias_fn".to_vec(), "ext")]
        );
        assert_eq!(t.origin(b"alias_fn"), Some("ext"));
        assert_eq!(t.origin(b"first"), Some("ext"));
    }

    #[test]
    fn a_fresh_key_re_attributes_the_origin_but_not_a_re_registration() {
        let mut t = Attributed::default();
        t.note_module(&ZStr::from("f"), "one", Some("one"));
        // Re-registered by another module without being a new key: the
        // module changes, Reflection's attribution stays.
        t.note_module(&ZStr::from("f"), "two", None);
        assert_eq!(listed(&t), [(b"f".to_vec(), "two")]);
        assert_eq!(t.origin(b"f"), Some("one"));
        // Inserted as a new key again (removed in between): the insert wins.
        t.note_origin(&ZStr::from("f"), "three");
        assert_eq!(t.origin(b"f"), Some("three"));
        assert_eq!(t.index[b"f".as_slice()].pos, Some(0));
        assert_eq!(t.origin(b"g"), None);
    }

    #[test]
    fn a_forgotten_registry_starts_empty_and_still_records() {
        // The registry is thread-local and each test runs on its own thread.
        super::set_current_module(Some("ext"));
        super::note_new_function(&ZStr::from("forgotten_fn"));
        super::note_new_class(&ZStr::from("forgottenclass"));
        super::note_constant(&ZStr::from("FORGOTTEN_C"), "ext", true);
        super::set_current_module(None);
        assert_eq!(super::function_origin(b"forgotten_fn"), Some("ext"));
        assert_eq!(super::class_origin(b"forgottenclass"), Some("ext"));
        assert_eq!(super::constant_origin(b"FORGOTTEN_C"), Some("ext"));
        super::forget_registry();
        assert_eq!(super::function_origin(b"forgotten_fn"), None);
        assert_eq!(super::class_origin(b"forgottenclass"), None);
        assert_eq!(super::constant_origin(b"FORGOTTEN_C"), None);
        super::set_current_module(Some("two"));
        super::note_function(&ZStr::from("again"), "two", true);
        super::set_current_module(None);
        assert_eq!(super::function_origin(b"again"), Some("two"));
    }
}

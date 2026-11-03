//! `Reflection`, `Reflector`, `ReflectionException`, `ReflectionGenerator`,
//! `ReflectionFiber`, `ReflectionReference`, `ReflectionExtension`,
//! `ReflectionZendExtension`, the `PropertyHookType` enum, and the
//! attribution of internal functions/classes/constants to extensions.

use super::{rexc, set_name_props, str_arg, this, this_refl, zs, FnRef, Refl, PRIV, PUB, RO, STAT};
use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, Class, ClassConst, ConstExpr, InternalKind, Magic, PropInfo};
use crate::func::{flags, Args, Builtin, Func, Method};
use crate::object;
use crate::types::{mask, TypeDecl};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, Value, ZArray, ZStr};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

// ---------------------------------------------------------------------
// Extension attribution
// ---------------------------------------------------------------------

/// Snapshot of the function, class and constant tables.
type Tables = (HashSet<Vec<u8>>, HashSet<Vec<u8>>, HashSet<Vec<u8>>);

#[derive(Default)]
struct Modules {
    functions: HashMap<Vec<u8>, String>,
    classes: HashMap<Vec<u8>, String>,
    constants: HashMap<Vec<u8>, String>,
    #[allow(dead_code)]
    snapshot: Option<Tables>,
}

thread_local! {
    static MODULES: RefCell<Modules> = RefCell::new(Modules::default());
}

#[allow(dead_code)]
fn snapshot(vm: &Vm) -> Tables {
    (
        vm.functions.keys().map(|k| k.as_bytes().to_vec()).collect(),
        vm.classes.keys().map(|k| k.as_bytes().to_vec()).collect(),
        vm.constants.keys().map(|k| k.as_bytes().to_vec()).collect(),
    )
}

/// Records which functions, classes and constants extension `name`
/// registered (call with `begin = true` before and `false` after
/// `Extension::register`).
#[allow(dead_code)] // called from `Vm::with_extensions` once that hook lands
pub(crate) fn record_module(vm: &Vm, name: &str, begin: bool) {
    if begin {
        let s = snapshot(vm);
        MODULES.with(|m| m.borrow_mut().snapshot = Some(s));
        return;
    }
    let (f, c, k) = snapshot(vm);
    MODULES.with(|m| {
        let mut m = m.borrow_mut();
        let Some((bf, bc, bk)) = m.snapshot.take() else {
            return;
        };
        for x in f.difference(&bf) {
            m.functions.insert(x.clone(), name.to_string());
        }
        for x in c.difference(&bc) {
            m.classes.insert(x.clone(), name.to_string());
        }
        for x in k.difference(&bk) {
            m.constants.insert(x.clone(), name.to_string());
        }
    });
}

/// Core (`zend_builtin_functions.c`) functions.
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
    "set_exception_handler",
    "restore_exception_handler",
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
    "gc_mem_caches",
    "gc_collect_cycles",
    "gc_enabled",
    "gc_enable",
    "gc_disable",
    "gc_status",
    "clone",
];

/// The extension of an internal function.
pub(crate) fn function_module(
    vm: &Vm,
    f: &Func,
    scope: Option<&Rc<Class>>,
    name: &ZStr,
) -> Option<String> {
    if f.is_user() {
        return None;
    }
    if let Some(c) = scope {
        return class_module(vm, c);
    }
    let lc = name.to_ascii_lowercase();
    if let Some(m) = MODULES.with(|m| m.borrow().functions.get(lc.as_bytes()).cloned()) {
        return Some(m);
    }
    let s = String::from_utf8_lossy(&lc).into_owned();
    if CORE_FUNCTIONS.contains(&s.as_str()) {
        return Some("Core".into());
    }
    let prefixed = [
        ("preg_", "pcre"),
        ("hash", "hash"),
        ("mt_", "random"),
        ("random_", "random"),
        ("spl_", "SPL"),
        ("iterator_", "SPL"),
        ("class_implements", "SPL"),
        ("class_parents", "SPL"),
        ("class_uses", "SPL"),
        ("date", "date"),
        ("timezone_", "date"),
        ("strtotime", "date"),
        ("mktime", "date"),
        ("gmmktime", "date"),
        ("checkdate", "date"),
        ("gmdate", "date"),
        ("idate", "date"),
        ("time", "date"),
        ("localtime", "date"),
        ("getdate", "date"),
    ];
    for (p, m) in prefixed {
        if s.starts_with(p) {
            return Some(m.into());
        }
    }
    if matches!(
        s.as_str(),
        "rand" | "srand" | "getrandmax" | "lcg_value" | "random_int" | "random_bytes"
    ) {
        return Some("random".into());
    }
    Some("standard".into())
}

/// The extension of an internal class.
pub(crate) fn class_module(vm: &Vm, c: &Rc<Class>) -> Option<String> {
    if c.flags & cflags::INTERNAL == 0 {
        return None;
    }
    if let Some(m) = MODULES.with(|m| m.borrow().classes.get(c.lc_name.as_bytes()).cloned()) {
        return Some(m);
    }
    let _ = vm;
    let n = c.lc_name.to_string_lossy();
    if n.starts_with("reflect") || n == "propertyhooktype" {
        return Some("Reflection".into());
    }
    if n.starts_with("random\\") {
        return Some("random".into());
    }
    if n.starts_with("date") {
        return Some("date".into());
    }
    let spl = [
        "arrayiterator",
        "arrayobject",
        "splobjectstorage",
        "spldoublylinkedlist",
        "splqueue",
        "splstack",
        "splheap",
        "splminheap",
        "splmaxheap",
        "splpriorityqueue",
        "splfixedarray",
        "splobserver",
        "splsubject",
        "splfileinfo",
        "splfileobject",
        "spltempfileobject",
        "directoryiterator",
        "filesystemiterator",
        "recursivedirectoryiterator",
        "globiterator",
        "logicexception",
        "badfunctioncallexception",
        "badmethodcallexception",
        "domainexception",
        "invalidargumentexception",
        "lengthexception",
        "outofrangeexception",
        "runtimeexception",
        "outofboundsexception",
        "overflowexception",
        "rangeexception",
        "underflowexception",
        "unexpectedvalueexception",
        "recursiveiterator",
        "outeriterator",
        "seekableiterator",
        "countable",
    ];
    if spl.contains(&n.as_str()) || n.ends_with("iterator") && !matches!(n.as_str(), "iterator") {
        return Some(if n == "countable" {
            "Core".into()
        } else {
            "SPL".into()
        });
    }
    Some("Core".into())
}

/// The extension of an internal constant.
pub(crate) fn constant_module(_vm: &Vm, name: &ZStr) -> String {
    if let Some(m) = MODULES.with(|m| m.borrow().constants.get(name.as_bytes()).cloned()) {
        return m;
    }
    let s = name.to_string_lossy();
    let core = s.starts_with("E_")
        || s.starts_with("ZEND_")
        || matches!(
            s.as_str(),
            "TRUE"
                | "FALSE"
                | "NULL"
                | "true"
                | "false"
                | "null"
                | "DEBUG_BACKTRACE_PROVIDE_OBJECT"
                | "DEBUG_BACKTRACE_IGNORE_ARGS"
                | "UPLOAD_ERR_OK"
        )
        || (s.starts_with("PHP_")
            && !s.starts_with("PHP_ROUND")
            && !s.starts_with("PHP_URL")
            && !s.starts_with("PHP_QUERY"));
    if core {
        "Core".into()
    } else if s.starts_with("PREG_") || s.starts_with("PCRE_") {
        "pcre".into()
    } else if s.starts_with("MT_RAND") {
        "random".into()
    } else {
        "standard".into()
    }
}

/// Whether a global constant is deprecated.
pub(crate) fn constant_deprecated(_name: &ZStr) -> bool {
    false
}

/// The file a user constant was declared in (not tracked).
pub(crate) fn constant_file(_name: &ZStr) -> Option<ZStr> {
    None
}

/// All extension names, as `get_loaded_extensions()` reports them.
fn module_names(vm: &Vm) -> Vec<String> {
    let mut v: Vec<String> = crate::LOADED_EXTENSIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    v.extend(vm.loaded_extensions.iter().cloned());
    v
}

/// `reflection_extension_factory`.
pub(crate) fn extension_value(vm: &mut Vm, name: &str) -> VmResult<Value> {
    let n = ZStr::from(name);
    super::new_refl(
        vm,
        "ReflectionExtension",
        Refl::Extension(n.clone()),
        Some(&n),
        None,
    )
}

// ---------------------------------------------------------------------
// Reflection
// ---------------------------------------------------------------------

fn get_modifier_names(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = a.at(0).as_int().unwrap_or(0);
    let mut out = Vec::new();
    if m & 64 != 0 {
        out.push(zs("abstract"));
    }
    if m & 32 != 0 {
        out.push(zs("final"));
    }
    if m & 512 != 0 {
        out.push(zs("virtual"));
    }
    match m & 7 {
        1 => out.push(zs("public")),
        4 => out.push(zs("private")),
        2 => out.push(zs("protected")),
        _ => {}
    }
    match m & 0x1c00 {
        0x800 => out.push(zs("protected(set)")),
        0x1000 => out.push(zs("private(set)")),
        _ => {}
    }
    if m & 16 != 0 {
        out.push(zs("static"));
    }
    if m & (128 | 65536) != 0 {
        out.push(zs("readonly"));
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

/// Registers `ReflectionException`, `Reflection` and `Reflector`.
pub(crate) fn register_base(vm: &mut Vm) {
    ClassBuilder::new("ReflectionException")
        .extends(vm, "Exception")
        .register(vm);
    ClassBuilder::new("Reflection")
        .method(
            "getModifierNames(int $modifiers): array",
            STAT,
            get_modifier_names,
        )
        .register(vm);
    ClassBuilder::new("Reflector")
        .flags(cflags::INTERFACE | cflags::ABSTRACT)
        .implements(vm, &["Stringable"])
        .register(vm);
}

fn clone_error(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(rexc(vm, "Cannot clone object using __clone()"))
}

// ---------------------------------------------------------------------
// ReflectionGenerator
// ---------------------------------------------------------------------

fn this_gen(vm: &mut Vm, a: &Args) -> VmResult<Obj> {
    match this_refl(vm, a)? {
        Refl::Generator(g) => Ok(g),
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

/// Information about the executing frame of a generator.
struct GenFrame {
    file: ZStr,
    line: u32,
    func: Option<Func>,
    this: Option<Obj>,
    closure: Option<Obj>,
    scope: Option<Rc<Class>>,
}

fn gen_frame(vm: &Vm, g: &Obj) -> Option<GenFrame> {
    let suspended = object::internal::<crate::generator::GenData, _>(g, |d| {
        if d.state == crate::generator::GenState::Finished {
            return Some(None);
        }
        d.frame.as_ref().map(|f| {
            Some(GenFrame {
                file: f.code.filename.clone(),
                line: f.line(),
                func: f.func.clone(),
                this: f.this.clone(),
                closure: f.closure.clone(),
                scope: f.scope.clone(),
            })
        })
    })
    .flatten();
    match suspended {
        Some(x) => x,
        None => vm
            .frames
            .iter()
            .rev()
            .find(|f| f.generator.as_ref().is_some_and(|x| Obj::ptr_eq(x, g)))
            .map(|f| GenFrame {
                file: f.code.filename.clone(),
                line: f.line(),
                func: f.func.clone(),
                this: f.this.clone(),
                closure: f.closure.clone(),
                scope: f.scope.clone(),
            }),
    }
}

fn gen_closed(vm: &mut Vm) -> crate::vm::Throw {
    rexc(vm, "Cannot fetch information from a closed Generator")
}

fn rg_construct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let Value::Object(g) = a.at(0) else {
        return Ok(Value::Null);
    };
    object::set_internal(&o, Refl::Generator(g));
    Ok(Value::Null)
}

fn rg_get_executing_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    match gen_frame(vm, &g) {
        Some(f) => Ok(Value::Int(i64::from(f.line))),
        None => Err(gen_closed(vm)),
    }
}

fn rg_get_executing_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    match gen_frame(vm, &g) {
        Some(f) => Ok(Value::Str(f.file)),
        None => Err(gen_closed(vm)),
    }
}

fn rg_get_this(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    match gen_frame(vm, &g) {
        Some(f) => Ok(f.this.map_or(Value::Null, Value::Object)),
        None => Err(gen_closed(vm)),
    }
}

fn rg_get_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    let Some(f) = gen_frame(vm, &g) else {
        return Err(gen_closed(vm));
    };
    let Some(func) = f.func else {
        return Ok(Value::Null);
    };
    if let Some(c) = f.closure {
        return super::function_value(
            vm,
            FnRef {
                func,
                method: None,
                class: None,
                closure: Some(c),
            },
        );
    }
    if let Some(s) = f.scope {
        if let Some(m) = s.find_method(func.name().as_bytes()).cloned() {
            return super::method_value(vm, &m, &s);
        }
    }
    super::function_value(vm, FnRef::function(func))
}

fn rg_get_executing_generator(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    if gen_frame(vm, &g).is_none() {
        return Err(gen_closed(vm));
    }
    let cur = vm.gen_get_current(&g)?;
    Ok(Value::Object(cur))
}

fn rg_is_closed(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    Ok(Value::Bool(gen_frame(vm, &g).is_none()))
}

fn rg_get_trace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = this_gen(vm, a)?;
    let Some(f) = gen_frame(vm, &g) else {
        return Err(gen_closed(vm));
    };
    let mut t = HashTable::new();
    let mut e = HashTable::new();
    e.insert(ArrayKey::from("file"), Value::Str(f.file));
    e.insert(ArrayKey::from("line"), Value::Int(i64::from(f.line)));
    if let Some(func) = &f.func {
        e.insert(ArrayKey::from("function"), Value::Str(func.name().clone()));
        if let Some(s) = &f.scope {
            e.insert(ArrayKey::from("class"), Value::Str(s.name.clone()));
            if let Some(o) = &f.this {
                e.insert(ArrayKey::from("object"), Value::Object(o.clone()));
            }
            e.insert(
                ArrayKey::from("type"),
                zs(if f.this.is_some() { "->" } else { "::" }),
            );
        }
    }
    e.insert(ArrayKey::from("args"), Value::Array(ZArray::new()));
    let _ = t.push(Value::Array(ZArray::from_table(e)));
    Ok(Value::Array(ZArray::from_table(t)))
}

/// Registers `ReflectionGenerator`.
pub(crate) fn register_generator(vm: &mut Vm) {
    ClassBuilder::new("ReflectionGenerator")
        .flags(cflags::FINAL | cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .method("__construct(Generator $generator)", PUB, rg_construct)
        .method("getExecutingLine(): int", PUB, rg_get_executing_line)
        .method("getExecutingFile(): string", PUB, rg_get_executing_file)
        .method(
            "getTrace(int $options = DEBUG_BACKTRACE_PROVIDE_OBJECT): array",
            PUB,
            rg_get_trace,
        )
        .method(
            "getFunction(): ReflectionFunctionAbstract",
            PUB,
            rg_get_function,
        )
        .method("getThis(): ?object", PUB, rg_get_this)
        .method(
            "getExecutingGenerator(): Generator",
            PUB,
            rg_get_executing_generator,
        )
        .method("isClosed(): bool", PUB, rg_is_closed)
        .register(vm);
}

// ---------------------------------------------------------------------
// ReflectionFiber
// ---------------------------------------------------------------------

fn this_fiber(vm: &mut Vm, a: &Args) -> VmResult<Obj> {
    match this_refl(vm, a)? {
        Refl::Fiber(f) => Ok(f),
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

fn fiber_state(f: &Obj) -> Option<crate::fiber::FiberState> {
    object::internal::<crate::fiber::FiberData, _>(f, |d| d.state)
}

fn fiber_location(vm: &Vm, f: &Obj) -> Option<(ZStr, u32)> {
    let st = fiber_state(f)?;
    match st {
        crate::fiber::FiberState::Suspended => {
            object::internal::<crate::fiber::FiberData, _>(f, |d| {
                d.frames
                    .iter()
                    .rev()
                    .find(|fr| fr.kind != crate::vm::FrameKind::Builtin)
                    .map(|fr| (fr.code.filename.clone(), fr.line()))
            })
            .flatten()
        }
        crate::fiber::FiberState::Running => {
            let fr = vm
                .frames
                .iter()
                .rev()
                .find(|fr| fr.kind != crate::vm::FrameKind::Builtin)?;
            Some((fr.code.filename.clone(), fr.line()))
        }
        _ => None,
    }
}

fn fiber_check(vm: &mut Vm, f: &Obj) -> VmResult<()> {
    match fiber_state(f) {
        Some(crate::fiber::FiberState::Suspended | crate::fiber::FiberState::Running) => Ok(()),
        _ => Err(vm.make_throw(
            "Error",
            "Cannot fetch information from a fiber that has not been started or is terminated",
        )),
    }
}

fn rfb_construct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    if let Value::Object(f) = a.at(0) {
        object::set_internal(&o, Refl::Fiber(f));
    }
    Ok(Value::Null)
}

fn rfb_get_fiber(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Object(this_fiber(vm, a)?))
}

fn rfb_get_executing_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = this_fiber(vm, a)?;
    fiber_check(vm, &f)?;
    Ok(fiber_location(vm, &f).map_or(Value::Null, |(file, _)| Value::Str(file)))
}

fn rfb_get_executing_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = this_fiber(vm, a)?;
    fiber_check(vm, &f)?;
    Ok(fiber_location(vm, &f).map_or(Value::Null, |(_, l)| Value::Int(i64::from(l))))
}

fn rfb_get_callable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = this_fiber(vm, a)?;
    if matches!(fiber_state(&f), Some(crate::fiber::FiberState::Dead) | None) {
        return Err(vm.make_throw(
            "Error",
            "Cannot fetch the callable from a fiber that has terminated",
        ));
    }
    Ok(Value::Null)
}

fn rfb_get_trace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = this_fiber(vm, a)?;
    fiber_check(vm, &f)?;
    if fiber_state(&f) == Some(crate::fiber::FiberState::Running) {
        let t = vm.backtrace(false, false, true, 0);
        return Ok(Value::Array(t));
    }
    Ok(Value::Array(ZArray::new()))
}

/// Registers `ReflectionFiber`.
pub(crate) fn register_fiber(vm: &mut Vm) {
    ClassBuilder::new("ReflectionFiber")
        .flags(cflags::FINAL | cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .method("__construct(Fiber $fiber)", PUB, rfb_construct)
        .method("getFiber(): Fiber", PUB, rfb_get_fiber)
        .method("getExecutingFile(): ?string", PUB, rfb_get_executing_file)
        .method("getExecutingLine(): ?int", PUB, rfb_get_executing_line)
        .method("getCallable(): callable", PUB, rfb_get_callable)
        .method(
            "getTrace(int $options = DEBUG_BACKTRACE_PROVIDE_OBJECT): array",
            PUB,
            rfb_get_trace,
        )
        .register(vm);
}

// ---------------------------------------------------------------------
// ReflectionReference
// ---------------------------------------------------------------------

thread_local! {
    static REF_KEY: Cell<u64> = const { Cell::new(0) };
}

fn rr_from_array_element(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Array(arr) = a.at(0) else {
        return Ok(Value::Null);
    };
    let key = match a.at(1) {
        Value::Int(i) => ArrayKey::Int(i),
        Value::Str(s) => ArrayKey::from_zstr(&s),
        _ => return Ok(Value::Null),
    };
    match arr.get(&key) {
        None => Err(rexc(vm, "Array key not found")),
        Some(Value::Reference(r)) => {
            let self_ref = matches!(&*r.borrow(), Value::Array(x) if ZArray::ptr_eq(x, &arr));
            if r.refcount() == 1 && !self_ref {
                return Ok(Value::Null);
            }
            super::new_refl(
                vm,
                "ReflectionReference",
                Refl::Reference(r.clone()),
                None,
                None,
            )
        }
        Some(_) => Ok(Value::Null),
    }
}

fn rr_get_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Refl::Reference(r) = this_refl(vm, a)? else {
        return Err(rexc(vm, "Corrupted ReflectionReference object"));
    };
    let key = REF_KEY.with(|k| {
        if k.get() == 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos());
            k.set(u64::try_from(now & 0xffff_ffff_ffff).unwrap_or(1) | 1);
        }
        k.get()
    });
    let mut data = key.to_le_bytes().to_vec();
    data.extend_from_slice(&(r.addr() as u64).to_le_bytes());
    let digest = php_hash::sha1(&data);
    Ok(Value::Str(ZStr::from(digest.to_vec())))
}

fn rr_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.make_throw(
        "Error",
        "Call to private ReflectionReference::__construct() from global scope",
    ))
}

/// Registers `ReflectionReference`.
pub(crate) fn register_reference(vm: &mut Vm) {
    ClassBuilder::new("ReflectionReference")
        .flags(cflags::FINAL | cflags::NOT_SERIALIZABLE | cflags::NO_NEW)
        .method(
            "fromArrayElement(array $array, int|string $key): ?ReflectionReference",
            STAT,
            rr_from_array_element,
        )
        .method("getId(): string", PUB, rr_get_id)
        .method("__clone(): void", PRIV, clone_error)
        .method("__construct()", PRIV, rr_construct)
        .register(vm);
}

// ---------------------------------------------------------------------
// ReflectionExtension / ReflectionZendExtension
// ---------------------------------------------------------------------

fn this_ext(vm: &mut Vm, a: &Args) -> VmResult<String> {
    match this_refl(vm, a)? {
        Refl::Extension(n) => Ok(n.to_string_lossy()),
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

fn re_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let n = str_arg(a, 0).to_string_lossy();
    let Some(m) = module_names(vm)
        .into_iter()
        .find(|m| m.eq_ignore_ascii_case(&n))
    else {
        return Err(rexc(vm, &format!("Extension \"{n}\" does not exist")));
    };
    let z = ZStr::from(m.as_str());
    set_name_props(&o, Some(&z), None);
    object::set_internal(&o, Refl::Extension(z));
    Ok(Value::Null)
}

fn re_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(zs(&this_ext(vm, a)?))
}

fn re_get_version(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let _ = this_ext(vm, a)?;
    Ok(zs(crate::PHP_VERSION))
}

fn ext_functions(vm: &Vm, m: &str) -> Vec<(ZStr, Func)> {
    let mut v: Vec<(ZStr, Func)> = vm
        .functions
        .iter()
        .filter(|(_, f)| !f.is_user())
        .filter(|(k, f)| function_module(vm, f, None, k).is_some_and(|x| x.eq_ignore_ascii_case(m)))
        .map(|(k, f)| (k.clone(), f.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn re_get_functions(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    let mut t = HashTable::new();
    for (k, f) in ext_functions(vm, &m) {
        let name = f.name().clone();
        let v = super::function_value(vm, FnRef::function(f))?;
        let _ = k;
        t.insert(ArrayKey::Str(name), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn ext_constants(vm: &Vm, m: &str) -> Vec<(ZStr, Value)> {
    let mut v: Vec<(ZStr, Value)> = vm
        .constants
        .iter()
        .filter(|(k, _)| !vm.user_constants.contains(k))
        .filter(|(k, _)| constant_module(vm, k).eq_ignore_ascii_case(m))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn re_get_constants(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    let mut t = HashTable::new();
    for (k, v) in ext_constants(vm, &m) {
        t.insert(ArrayKey::Str(k), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn re_get_ini_entries(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    let mut t = HashTable::new();
    if let Ok(entries) = vm.ini.ini_get_all(Some(&m)) {
        for e in entries {
            let v = e
                .local_value
                .map_or(Value::Null, |x| Value::Str(ZStr::from(x)));
            t.insert(ArrayKey::Str(ZStr::from(e.name.as_str())), v);
        }
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn ext_classes(vm: &Vm, m: &str) -> Vec<Rc<Class>> {
    vm.class_order
        .iter()
        .filter_map(|lc| vm.classes.get(lc).cloned())
        .filter(|c| c.flags & cflags::INTERNAL != 0)
        .filter(|c| class_module(vm, c).is_some_and(|x| x.eq_ignore_ascii_case(m)))
        .collect()
}

fn re_get_classes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    let mut t = HashTable::new();
    for c in ext_classes(vm, &m) {
        let v = super::class_value(vm, &c)?;
        t.insert(ArrayKey::Str(c.name.clone()), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn re_get_class_names(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    Ok(Value::Array(ZArray::from_values(
        ext_classes(vm, &m)
            .into_iter()
            .map(|c| Value::Str(c.name.clone())),
    )))
}

fn re_get_dependencies(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    let deps: &[(&str, &str)] = match m.to_ascii_lowercase().as_str() {
        "spl" => &[("pcre", "Required"), ("standard", "Required")],
        "standard" => &[("session", "Optional")],
        "dom" => &[("libxml", "Required")],
        "simplexml" => &[("libxml", "Required"), ("spl", "Required")],
        "xml" | "xmlreader" | "xmlwriter" => &[("libxml", "Required")],
        "pdo_sqlite" => &[("pdo", "Required")],
        "mysqli" => &[("mysqlnd", "Required")],
        _ => &[],
    };
    let mut t = HashTable::new();
    for (k, v) in deps {
        t.insert(ArrayKey::from(*k), zs(v));
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn re_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    vm.echo(format!("\n{m}\n\n").as_bytes());
    Ok(Value::Null)
}

fn re_is_persistent(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let _ = this_ext(vm, a)?;
    Ok(Value::Bool(true))
}

fn re_is_temporary(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let _ = this_ext(vm, a)?;
    Ok(Value::Bool(false))
}

fn re_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = this_ext(vm, a)?;
    let num = module_names(vm)
        .iter()
        .position(|x| x.eq_ignore_ascii_case(&m))
        .map_or(0, |i| i + 1);
    let mut out = format!(
        "Extension [ <persistent> extension #{num} {m} version {} ] {{\n",
        crate::PHP_VERSION
    )
    .into_bytes();
    if let Ok(entries) = vm.ini.ini_get_all(Some(&m)) {
        if !entries.is_empty() {
            out.extend_from_slice(b"\n  - INI {\n");
            for e in entries {
                let acc = match e.access {
                    7 => "ALL".to_string(),
                    x => {
                        let mut p = Vec::new();
                        if x & 1 != 0 {
                            p.push("USER");
                        }
                        if x & 2 != 0 {
                            p.push("PERDIR");
                        }
                        if x & 4 != 0 {
                            p.push("SYSTEM");
                        }
                        p.join(",")
                    }
                };
                out.extend_from_slice(
                    format!("    Entry [ {} <{acc}> ]\n      Current = '", e.name).as_bytes(),
                );
                out.extend_from_slice(e.local_value.as_deref().unwrap_or_default());
                out.extend_from_slice(b"'\n");
                if e.local_value != e.global_value {
                    out.extend_from_slice(b"      Default = '");
                    out.extend_from_slice(e.global_value.as_deref().unwrap_or_default());
                    out.extend_from_slice(b"'\n");
                }
                out.extend_from_slice(b"    }\n");
            }
            out.extend_from_slice(b"  }\n");
        }
    }
    let consts = ext_constants(vm, &m);
    if !consts.is_empty() {
        out.extend_from_slice(format!("\n  - Constants [{}] {{\n", consts.len()).as_bytes());
        for (k, v) in consts {
            out.extend(super::export::const_string(
                vm,
                &k,
                &v,
                &["persistent"],
                "    ",
            )?);
        }
        out.extend_from_slice(b"  }\n");
    }
    let funcs = ext_functions(vm, &m);
    if !funcs.is_empty() {
        out.extend_from_slice(b"\n  - Functions {\n");
        for (_, f) in funcs {
            out.extend(super::export::function_string(
                vm,
                &FnRef::function(f),
                None,
                "    ",
            ));
        }
        out.extend_from_slice(b"  }\n");
    }
    let classes = ext_classes(vm, &m);
    if !classes.is_empty() {
        out.extend_from_slice(format!("\n  - Classes [{}] {{", classes.len()).as_bytes());
        for c in classes {
            out.push(b'\n');
            out.extend(super::export::class_string(vm, &c, None, "    ")?);
        }
        out.extend_from_slice(b"  }\n");
    }
    out.extend_from_slice(b"}\n");
    Ok(Value::Str(ZStr::from(out)))
}

fn rze_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = str_arg(a, 0);
    Err(rexc(
        vm,
        &format!("Zend Extension \"{}\" does not exist", n.to_string_lossy()),
    ))
}

fn rze_empty(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let _ = this_ext(vm, a)?;
    Ok(zs(""))
}

/// Registers `ReflectionExtension` and `ReflectionZendExtension`.
pub(crate) fn register_extension(vm: &mut Vm) {
    ClassBuilder::new("ReflectionExtension")
        .implements(vm, &["Reflector"])
        .flags(cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .hooks(super::constant::hooks())
        .prop("name", RO, Some("string"), None)
        .method("__clone(): void", PRIV, clone_error)
        .method("__construct(string $name)", PUB, re_construct)
        .method("__toString(): string", PUB, re_to_string)
        .method("getName(): string", PUB, re_get_name)
        .method("getVersion(): ?string", PUB, re_get_version)
        .method("getFunctions(): array", PUB, re_get_functions)
        .method("getConstants(): array", PUB, re_get_constants)
        .method("getINIEntries(): array", PUB, re_get_ini_entries)
        .method("getClasses(): array", PUB, re_get_classes)
        .method("getClassNames(): array", PUB, re_get_class_names)
        .method("getDependencies(): array", PUB, re_get_dependencies)
        .method("info(): void", PUB, re_info)
        .method("isPersistent(): bool", PUB, re_is_persistent)
        .method("isTemporary(): bool", PUB, re_is_temporary)
        .register(vm);
    ClassBuilder::new("ReflectionZendExtension")
        .implements(vm, &["Reflector"])
        .flags(cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .hooks(super::constant::hooks())
        .prop("name", RO, Some("string"), None)
        .method("__clone(): void", PRIV, clone_error)
        .method("__construct(string $name)", PUB, rze_construct)
        .method("__toString(): string", PUB, rze_empty)
        .method("getName(): string", PUB, rze_empty)
        .method("getVersion(): string", PUB, rze_empty)
        .method("getAuthor(): string", PUB, rze_empty)
        .method("getURL(): string", PUB, rze_empty)
        .method("getCopyright(): string", PUB, rze_empty)
        .register(vm);
}

// ---------------------------------------------------------------------
// PropertyHookType
// ---------------------------------------------------------------------

/// Registers the internal backed enum `PropertyHookType`.
pub(crate) fn register_hook_type(vm: &mut Vm) {
    let name = ZStr::from("PropertyHookType");
    let ifaces: Vec<Rc<Class>> = ["UnitEnum", "BackedEnum"]
        .iter()
        .filter_map(|n| vm.find_class(n.as_bytes()))
        .collect();
    let cls = Rc::new_cyclic(|weak| {
        let mk_const = |n: &str, v: &str| {
            Rc::new(ClassConst {
                name: ZStr::from(n),
                value: RefCell::new(None),
                expr: ConstExpr::Value(zs(v)),
                flags: flags::PUBLIC,
                class: name.clone(),
                ty: None,
                is_case: true,
                evaluating: Cell::new(false),
                doc_comment: None,
                attributes: Vec::new(),
            })
        };
        let consts = vec![mk_const("Get", "get"), mk_const("Set", "set")];
        let const_map: HashMap<ZStr, usize> = consts
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let mk_prop = |n: &str, slot: u32| {
            Rc::new(PropInfo {
                name: ZStr::from(n),
                slot,
                flags: flags::PUBLIC | flags::READONLY,
                ty: Some(TypeDecl::simple(mask::STRING)),
                class: name.clone(),
                default: None,
                doc_comment: None,
                attributes: Vec::new(),
                hooks: None,
            })
        };
        let props = vec![mk_prop("name", 0), mk_prop("value", 1)];
        let prop_map: HashMap<ZStr, Rc<PropInfo>> =
            props.iter().map(|p| (p.name.clone(), p.clone())).collect();
        let mut methods: HashMap<ZStr, Rc<Method>> = HashMap::new();
        let mut order = Vec::new();
        for (sig, fl, h) in crate::builtins::enum_methods() {
            let s = crate::builtins::sig::parse_sig(sig);
            let lc = ZStr::from(s.name.to_ascii_lowercase());
            let b = Builtin {
                name: ZStr::from(s.name.as_str()),
                params: s.params,
                defaults: s.defaults,
                required: s.required,
                return_type: s.ret,
                flags: fl,
                handler: h,
            };
            order.push(lc.clone());
            methods.insert(
                lc,
                Rc::new(Method {
                    name: ZStr::from(s.name.as_str()),
                    func: Func::Builtin(Rc::new(b)),
                    flags: fl,
                    scope: weak.clone(),
                    proto_scope: weak.clone(),
                }),
            );
        }
        for i in &ifaces {
            for lc in &i.method_order {
                if !methods.contains_key(lc) {
                    if let Some(m) = i.methods.get(lc) {
                        order.push(lc.clone());
                        methods.insert(lc.clone(), m.clone());
                    }
                }
            }
        }
        let magic: Magic = crate::classlink::build_magic(&methods);
        Class {
            name: name.clone(),
            lc_name: ZStr::from("propertyhooktype"),
            flags: cflags::INTERNAL | cflags::ENUM | cflags::FINAL | cflags::NO_NEW,
            parent: None,
            interfaces: ifaces.clone(),
            traits: Vec::new(),
            consts,
            const_map,
            props,
            prop_map,
            static_props: Vec::new(),
            static_map: HashMap::new(),
            methods,
            method_order: order,
            magic,
            defaults: RefCell::new(None),
            backing: Some(mask::STRING),
            enum_cases: RefCell::new(HashMap::new()),
            decl: None,
            internal: InternalKind::None,
            attributes: Vec::new(),
            filename: None,
            line_start: 0,
            line_end: 0,
            doc_comment: None,
            linked: Cell::new(true),
            hooks: None,
        }
    });
    vm.register_class(cls);
}

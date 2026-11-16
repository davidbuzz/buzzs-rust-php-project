// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `XSLTProcessor` class: registration, object handlers and methods
//! (`ext/xsl/xsltprocessor.c`, `ext/xsl/php_xsl.c`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use php_runtime::{ArrayKey, Obj, Value, ZStr};
use php_vm::class::Class;
use php_vm::ext::{acc, Args, ClassBuilder, ClassHooks, PropCtx};
use php_vm::object::{self, oflags};
use php_vm::{Throw, Vm, VmResult};
use php_xsl::{ParamKey, XmlOutput, XslError, XsltProcessor, PROPERTY_NAMES};

use crate::access::{self, DomAccess, ImportedNode, XmlSource};
use php_ext_xml::libxml;

use crate::host::{throw_xsl, BridgeHost};

/// The library processor, specialised to PHP callables.
type Processor = XsltProcessor<Value>;

/// Native state of an `XSLTProcessor` object.
struct State(Rc<Processor>);

/// An error raised either by the library or by the VM (conversions that
/// may call user code).
enum Fail {
    Xsl(XslError),
    Vm(Throw),
}

impl From<XslError> for Fail {
    fn from(e: XslError) -> Self {
        Self::Xsl(e)
    }
}

impl Fail {
    fn into_throw(self, vm: &mut Vm) -> Throw {
        match self {
            Self::Xsl(e) => throw_xsl(vm, &e),
            Self::Vm(t) => t,
        }
    }
}

/// The processor of `$this`. Every method checks it through here; an
/// object without state cannot exist (the `create` hook always runs).
fn processor(vm: &mut Vm, a: &Args) -> VmResult<(Obj, Rc<Processor>)> {
    let this = a.this_obj();
    match object::internal::<State, _>(&this, |s| s.0.clone()) {
        Some(p) => Ok((this, p)),
        None => Err(vm.throw("Error", "Invalid XSLTProcessor object")),
    }
}

fn xsl<T>(vm: &mut Vm, r: Result<T, XslError>) -> VmResult<T> {
    r.map_err(|e| throw_xsl(vm, &e))
}

// ---------------------------------------------------------------- properties

const MAX_TEMPLATE_DEPTH: &str = "maxTemplateDepth";
const MAX_TEMPLATE_VARS: &str = "maxTemplateVars";

fn is_validated(name: &[u8]) -> bool {
    name == MAX_TEMPLATE_DEPTH.as_bytes() || name == MAX_TEMPLATE_VARS.as_bytes()
}

/// The raw value of a declared property slot (`None` when unset).
fn slot(o: &Obj, name: &str) -> Option<Value> {
    let cls = object::class_of(o);
    let pi = cls.prop_map.get(&ZStr::from(name))?;
    let d = object::data(o);
    let slots = d.slots.borrow();
    slots
        .get(usize::try_from(pi.slot).ok()?)?
        .as_ref()
        .map(Value::deref)
}

fn set_slot(o: &Obj, name: &str, v: Value) {
    let cls = object::class_of(o);
    let Some(pi) = cls.prop_map.get(&ZStr::from(name)) else {
        return;
    };
    let d = object::data(o);
    let mut slots = d.slots.borrow_mut();
    if let Some(s) = usize::try_from(pi.slot).ok().and_then(|i| slots.get_mut(i)) {
        match s {
            Some(Value::Reference(r)) => {
                r.set(v);
            }
            other => *other = Some(v),
        }
    }
}

/// Copies the declared properties into the library processor (PHP reads
/// them from the property table at import/transformation time).
fn sync_properties(vm: &mut Vm, o: &Obj, p: &Processor) -> VmResult<()> {
    let flag = |vm: &mut Vm, name: &str| -> VmResult<Option<bool>> {
        match slot(o, name) {
            None => Ok(None),
            Some(v) => vm.value_to_bool(&v).map(Some),
        }
    };
    p.set_do_xinclude(flag(vm, PROPERTY_NAMES[0])?);
    p.set_clone_document(flag(vm, PROPERTY_NAMES[1])?);
    let defaults = p.properties();
    let int = |name: &str, default: i64| match slot(o, name) {
        Some(Value::Int(i)) if i >= 0 => i,
        _ => default,
    };
    let _ = p.set_max_template_depth(int(MAX_TEMPLATE_DEPTH, defaults.max_template_depth));
    let _ = p.set_max_template_vars(int(MAX_TEMPLATE_VARS, defaults.max_template_vars));
    Ok(())
}

thread_local! {
    /// Set while the standard write handler runs on behalf of
    /// [`write_property`] (`zend_std_write_property` from the custom
    /// handler).
    static STD_WRITE: Cell<bool> = const { Cell::new(false) };
}

struct StdWriteGuard;

impl StdWriteGuard {
    fn enter() -> Self {
        STD_WRITE.with(|c| c.set(true));
        Self
    }
}

impl Drop for StdWriteGuard {
    fn drop(&mut self) {
        STD_WRITE.with(|c| c.set(false));
    }
}

/// `xsl_objects_write_property`: `$maxTemplateDepth`/`$maxTemplateVars`
/// are written by the standard handler (with its type coercion), then a
/// negative result is rolled back with a `ValueError`.
fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, v: &Value) -> VmResult<bool> {
    if STD_WRITE.with(Cell::get) || !is_validated(name.as_bytes()) {
        return Ok(false);
    }
    let prop = if name.as_bytes() == MAX_TEMPLATE_DEPTH.as_bytes() {
        MAX_TEMPLATE_DEPTH
    } else {
        MAX_TEMPLATE_VARS
    };
    let old = slot(o, prop);
    {
        let _guard = StdWriteGuard::enter();
        vm.set_property(o, prop, v.clone())?;
    }
    if let Some(Value::Int(n)) = slot(o, prop) {
        if n < 0 {
            if let Some(old) = old {
                set_slot(o, prop, old);
            }
            return Err(throw_xsl(vm, &XslError::negative_limit(prop)));
        }
    }
    Ok(true)
}

/// `xsl_objects_get_property_ptr_ptr` + `xsl_objects_read_property`: the
/// validated properties cannot be fetched for writing (references, nested
/// writes/unsets), which would bypass the validation.
fn read_property(vm: &mut Vm, _o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    if is_validated(name.as_bytes())
        && matches!(
            vm.property_context(),
            PropCtx::Ref | PropCtx::Write | PropCtx::Unset
        )
    {
        let e = XslError::indirect_modification(&name.to_string_lossy());
        return Err(throw_xsl(vm, &e));
    }
    Ok(None)
}

/// `xsl_objects_unset_property`.
fn unset_property(vm: &mut Vm, _o: &Obj, name: &ZStr) -> VmResult<bool> {
    if is_validated(name.as_bytes()) {
        let e = XslError::cannot_unset(&name.to_string_lossy());
        return Err(throw_xsl(vm, &e));
    }
    Ok(false)
}

/// `xsl_objects_new`: native state and the libxslt-derived defaults.
#[allow(clippy::unnecessary_wraps)] // the `ClassHooks::create` signature
fn create(vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let p = Processor::new();
    let props = p.properties();
    vm.set_property_raw(o, MAX_TEMPLATE_DEPTH, Value::Int(props.max_template_depth));
    vm.set_property_raw(o, MAX_TEMPLATE_VARS, Value::Int(props.max_template_vars));
    object::set_internal(o, State(Rc::new(p)));
    Ok(())
}

// ------------------------------------------------------------------ inputs

/// The root of `cls`'s parent chain.
fn hierarchy_root(cls: &Rc<Class>) -> Rc<Class> {
    let mut root = cls.clone();
    while let Some(p) = root.parent.clone() {
        root = p;
    }
    root
}

/// `php_xsl_apply_stylesheet`'s input resolution: the document behind
/// argument #1.
fn input_document(
    vm: &mut Vm,
    a: &Args,
    method: &'static str,
) -> VmResult<(Obj, &'static dyn DomAccess, XmlSource)> {
    let Value::Object(obj) = a.at(0) else {
        return Err(throw_xsl(vm, &XslError::invalid_node(method)));
    };
    match access::import(vm, &obj)? {
        Some((provider, found)) => match found.into_source() {
            Some(src) => Ok((obj, provider, src)),
            None => Err(throw_xsl(vm, &XslError::invalid_node(method))),
        },
        None => Err(throw_xsl(vm, &XslError::invalid_node(method))),
    }
}

/// `clone $o` (`zend_objects_clone_obj` + the class's native clone hook +
/// `__clone()`), for `importStylesheet()`'s stylesheet clone.
fn clone_object(vm: &mut Vm, o: &Obj) -> VmResult<Obj> {
    let cls = object::class_of(o).clone();
    let n = vm.new_object(&cls)?;
    {
        let src = object::data(o);
        let dst = object::data(&n);
        let slots: Vec<Option<Value>> = src
            .slots
            .borrow()
            .iter()
            .map(|s| s.as_ref().map(Value::deref))
            .collect();
        // The new object's slots (as many: same class) take the copies.
        for (slot, v) in dst.slots.borrow_mut().iter_mut().zip(slots) {
            *slot = v;
        }
        let dynamic = src.dynamic.borrow().clone();
        *dst.dynamic.borrow_mut() = dynamic;
    }
    if let Some(f) = cls.hooks.as_ref().and_then(|h| h.clone) {
        f(vm, o, &n)?;
    }
    if cls.magic.clone.is_some() {
        let d = object::data(&n);
        d.flags.set(d.flags.get() | oflags::CLONING);
        let r = vm.call_method_by_name(&n, "__clone", Vec::new());
        d.flags.set(d.flags.get() & !oflags::CLONING);
        r?;
    }
    Ok(n)
}

// ----------------------------------------------------------------- methods

fn import_stylesheet(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    const M: &str = "importStylesheet";
    let (this, p) = processor(vm, a)?;
    if p.is_transforming() {
        return Err(vm.throw(
            "Error",
            "Cannot call XSLTProcessor::importStylesheet() while a transformation is in progress",
        ));
    }
    let Value::Object(obj) = a.at(0) else {
        return Err(throw_xsl(vm, &XslError::invalid_node(M)));
    };
    let Some((provider, found)) = access::import(vm, &obj)? else {
        return Err(throw_xsl(vm, &XslError::invalid_node(M)));
    };
    if object::class_of(&obj).flags & php_vm::ext::class_flags::NO_CLONE != 0 {
        return Err(throw_xsl(vm, &XslError::not_cloneable()));
    }
    let (is_document, source) = match found {
        ImportedNode::Detached => return Err(throw_xsl(vm, &XslError::not_in_document())),
        ImportedNode::Document(s) => (true, s),
        ImportedNode::Node(s) => (false, s),
    };
    // PHP clones the (owner) document object and compiles the clone. The
    // library compiles a private copy anyway, so the clone is only made
    // when user code can observe it (`__clone()`).
    let target = if is_document {
        obj.clone()
    } else {
        provider
            .owner_document(vm, &obj)?
            .unwrap_or_else(|| obj.clone())
    };
    let source = if object::class_of(&target).magic.clone.is_some() {
        drop(source);
        let clone = clone_object(vm, &target)?;
        if Rc::strong_count(clone.rc()) > 1 || provider.document_shared(&clone) {
            drop(clone);
            return Err(throw_xsl(vm, &XslError::clone_retained()));
        }
        match provider
            .import(vm, &clone)?
            .and_then(ImportedNode::into_source)
        {
            Some(s) => s,
            None => return Err(throw_xsl(vm, &XslError::invalid_node(M))),
        }
    } else {
        source
    };
    sync_properties(vm, &this, &p)?;
    let r = libxml::run(vm, a, || p.import_stylesheet(&source))?;
    drop(source);
    let ok = xsl(vm, r)?;
    Ok(Value::Bool(ok))
}

/// Runs a transformation with a [`BridgeHost`] inside `libxml::run`
/// (stream I/O, diagnostics); rethrows the exception raised during it, if
/// any.
fn run<T>(
    vm: &mut Vm,
    a: &Args,
    this: &Obj,
    p: &Processor,
    source: &XmlSource,
    f: impl FnOnce(&Processor, &mut BridgeHost<'_>) -> Result<T, XslError>,
) -> VmResult<T> {
    sync_properties(vm, this, p)?;
    let dom = access::provider("dom");
    let r = libxml::run(vm, a, || {
        let mut host = BridgeHost::new(source, dom);
        f(p, &mut host)
    })?;
    xsl(vm, r)
}

fn transform_to_doc(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    const M: &str = "transformToDoc";
    let (this, p) = processor(vm, a)?;
    // ZPP ("o|C!") resolves the class before anything else.
    let return_class = match a.opt(1) {
        Some(name) => {
            let name = vm.value_to_string(&name)?;
            let Some(class) = vm.lookup_class(&name)? else {
                let e = XslError::unknown_return_class(&name.to_string_lossy());
                return Err(throw_xsl(vm, &e));
            };
            Some(class)
        }
        None => None,
    };
    let (input, provider, source) = input_document(vm, a, M)?;
    let result = run(vm, a, &this, &p, &source, |p, h| {
        p.transform_to_doc(&source, h)
    })?;
    drop(source);
    let Some(doc) = result else {
        return Ok(Value::Bool(false));
    };
    match return_class {
        Some(cls) => {
            let input_class = object::class_of(&input).clone();
            let root = hierarchy_root(&input_class);
            if !cls.is_subclass_of(&root) {
                drop(doc);
                let e = XslError::incompatible_return_class(
                    &object::display_name(&input_class.name).to_string_lossy(),
                    &object::display_name(&cls.name).to_string_lossy(),
                );
                return Err(throw_xsl(vm, &e));
            }
            provider.wrap_document(vm, doc, Some(&cls))
        }
        None => match access::provider("dom") {
            Some(dom) => dom.wrap_document(vm, doc, None),
            None => Err(vm.throw(
                "Error",
                "XSLTProcessor::transformToDoc(): Cannot create a DOMDocument for the result: the dom extension is not available",
            )),
        },
    }
}

fn transform_to_uri(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    const M: &str = "transformToUri";
    let (this, p) = processor(vm, a)?;
    let uri = a.str(1);
    if uri.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 2, "must not contain any null bytes"));
    }
    let (_input, _provider, source) = input_document(vm, a, M)?;
    let n = run(vm, a, &this, &p, &source, |p, h| {
        p.transform_to_uri(&source, uri.as_bytes(), h)
    })?;
    Ok(Value::Int(n))
}

fn transform_to_xml(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    const M: &str = "transformToXml";
    let (this, p) = processor(vm, a)?;
    let (_input, _provider, source) = input_document(vm, a, M)?;
    let out = run(vm, a, &this, &p, &source, |p, h| {
        p.transform_to_xml(&source, h)
    })?;
    Ok(match out {
        XmlOutput::Str(s) => Value::Str(ZStr::from(s)),
        XmlOutput::Null => Value::Null,
        XmlOutput::False => Value::Bool(false),
    })
}

fn set_parameter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    let namespace = a.str(0);
    let value = a.opt(2).map(|v| match v {
        Value::Str(s) => s,
        _ => ZStr::from(""),
    });
    let value = value.as_ref().map(ZStr::as_bytes);
    match a.at(1) {
        Value::Array(entries) => {
            let list: Vec<(ParamKey, Value)> = entries
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        ArrayKey::Int(i) => ParamKey::Int(*i),
                        ArrayKey::Str(s) => ParamKey::Str(s.as_bytes().to_vec()),
                    };
                    (key, v.deref())
                })
                .collect();
            let vm_cell = RefCell::new(&mut *vm);
            let r = p.set_parameters(namespace.as_bytes(), list, value, |v: &Value| {
                vm_cell
                    .borrow_mut()
                    .value_to_string(v)
                    .map(|s| s.as_bytes().to_vec())
                    .map_err(Fail::Vm)
            });
            match r {
                Ok(b) => Ok(Value::Bool(b)),
                Err(f) => Err(f.into_throw(vm)),
            }
        }
        name => {
            let name = vm.value_to_string(&name)?;
            let r = p.set_parameter(namespace.as_bytes(), name.as_bytes(), value);
            xsl(vm, r).map(Value::Bool)
        }
    }
}

fn get_parameter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    let r = p.get_parameter(a.str(0).as_bytes(), a.str(1).as_bytes());
    Ok(match xsl(vm, r)? {
        Some(v) => Value::Str(ZStr::from(v)),
        None => Value::Bool(false),
    })
}

fn remove_parameter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    let r = p.remove_parameter(a.str(0).as_bytes(), a.str(1).as_bytes());
    xsl(vm, r).map(Value::Bool)
}

#[allow(clippy::unnecessary_wraps)] // the native method signature
fn has_exslt_support(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(Processor::has_exslt_support()))
}

/// `zend_is_callable` for a registration: the callable, or the error text.
fn resolve_callable(vm: &mut Vm, v: &Value) -> Result<Value, String> {
    if vm.is_callable(v) {
        Ok(v.clone())
    } else {
        Err(vm.callable_error_message(v))
    }
}

fn register_php_functions(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    match a.at(0) {
        Value::Null => p.register_php_functions_all(),
        Value::Array(entries) => {
            let list: Vec<(Option<Vec<u8>>, Value)> = entries
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        ArrayKey::Int(_) => None,
                        ArrayKey::Str(s) => Some(s.as_bytes().to_vec()),
                    };
                    (key, v.deref())
                })
                .collect();
            let vm_cell = RefCell::new(&mut *vm);
            let r = p.register_php_functions_array(
                list,
                |v| resolve_callable(&mut vm_cell.borrow_mut(), v),
                |v| {
                    vm_cell
                        .borrow_mut()
                        .value_to_string(v)
                        .map(|s| s.as_bytes().to_vec())
                        .map_err(Fail::Vm)
                },
            );
            if let Err(f) = r {
                return Err(f.into_throw(vm));
            }
        }
        other => {
            let name = vm.value_to_string(&other)?;
            let r = p.register_php_functions_name(name.as_bytes(), |n| {
                resolve_callable(vm, &Value::Str(ZStr::from(n)))
            });
            xsl(vm, r)?;
        }
    }
    Ok(Value::Null)
}

fn register_php_function_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    let callable = a.at(2);
    let r = p.register_php_function_ns(a.str(0).as_bytes(), a.str(1).as_bytes(), || {
        Ok::<_, XslError>(callable)
    });
    xsl(vm, r)?;
    Ok(Value::Null)
}

fn set_profiling(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    let file = a.opt(0).map(|_| a.str(0));
    let r = p.set_profiling(file.as_ref().map(ZStr::as_bytes));
    xsl(vm, r).map(Value::Bool)
}

fn set_security_prefs(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    Ok(Value::Int(p.set_security_prefs(a.int(0))))
}

fn get_security_prefs(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_this, p) = processor(vm, a)?;
    Ok(Value::Int(p.get_security_prefs()))
}

type Handler = fn(&mut Vm, &mut Args) -> VmResult<Value>;

/// The methods, in stub order (`php_xsl.stub.php`).
const METHODS: &[(&str, Handler)] = &[
    (
        "importStylesheet(object $stylesheet): bool",
        import_stylesheet,
    ),
    (
        "transformToDoc(object $document, ?string $returnClass = null): object|false",
        transform_to_doc,
    ),
    (
        "transformToUri(object $document, string $uri): int",
        transform_to_uri,
    ),
    (
        "transformToXml(object $document): string|null|false",
        transform_to_xml,
    ),
    (
        "setParameter(string $namespace, array|string $name, ?string $value = null): bool",
        set_parameter,
    ),
    (
        "getParameter(string $namespace, string $name): string|false",
        get_parameter,
    ),
    (
        "removeParameter(string $namespace, string $name): bool",
        remove_parameter,
    ),
    ("hasExsltSupport(): bool", has_exslt_support),
    (
        "registerPHPFunctions(array|string|null $functions = null): void",
        register_php_functions,
    ),
    (
        "registerPHPFunctionNS(string $namespaceURI, string $name, callable $callable): void",
        register_php_function_ns,
    ),
    ("setProfiling(?string $filename): true", set_profiling),
    (
        "setSecurityPrefs(int $preferences): int",
        set_security_prefs,
    ),
    ("getSecurityPrefs(): int", get_security_prefs),
];

/// Registers `XSLTProcessor`.
pub(crate) fn register_class(vm: &mut Vm) {
    let mut b = ClassBuilder::new("XSLTProcessor")
        .flags(php_vm::ext::class_flags::NO_CLONE)
        .prop(
            PROPERTY_NAMES[0],
            acc::PUBLIC,
            Some("bool"),
            Some(Value::Bool(false)),
        )
        .prop(
            PROPERTY_NAMES[1],
            acc::PUBLIC,
            Some("bool"),
            Some(Value::Bool(false)),
        )
        .prop(MAX_TEMPLATE_DEPTH, acc::PUBLIC, Some("int"), None)
        .prop(MAX_TEMPLATE_VARS, acc::PUBLIC, Some("int"), None);
    for (sig, handler) in METHODS {
        b = b.method(sig, acc::PUBLIC, *handler);
    }
    b.hooks(ClassHooks {
        create: Some(create),
        read_property: Some(read_property),
        write_property: Some(write_property),
        unset_property: Some(unset_property),
        ..ClassHooks::default()
    })
    .register(vm);
}

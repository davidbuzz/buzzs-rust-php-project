//! `ext/xmlreader` for the phprust VM: binds the `php-xmlreader` library
//! (the `XMLReader` class over libxml2's `xmlTextReader`) through the
//! `php_vm::ext` extension API.
//!
//! * The object state is a [`php_xmlreader::XmlReader`] behind an
//!   `Rc<RefCell<..>>` attached with [`php_vm::object::set_internal`] by
//!   the `create` hook (inherited by user subclasses).
//! * The fourteen read-only properties are virtual: they are served by the
//!   `read_property`/`has_property` hooks, rejected by
//!   `write_property`/`unset_property` and appended to the `var_dump()`
//!   table by `properties_for`.
//! * `open()`/`XML()` are static methods that PHP also lets you call on an
//!   instance (`xmlreader_get_method`): they are registered with
//!   `acc::STATIC_KEEPS_THIS`, so `$this` is set for instance calls.
//! * Every libxml2-backed call runs inside [`php_ext_xml::libxml::run`],
//!   which lends the VM to the stream layer and reports libxml2
//!   diagnostics as `XMLReader::method(): ...` warnings (or records them
//!   with `libxml_use_internal_errors(true)`).

use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

use php_ext_streams::res::{cell_of, StreamCell};
use php_runtime::{ArrayKey, HashTable, Obj, PropertyPurpose, Resource, Value, ZArray, ZStr};
use php_vm::class::Class;
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, Extension, ModuleBuilder, PropCtx, Throw,
    VmResult,
};
use php_vm::func::flags;
use php_vm::object::{class_of, data};
use php_vm::Vm;
use php_xmlreader::{
    CollectingHost, IssetKind, PropValue, Property, ReaderError, Source, XmlReader,
};

/// The `xmlreader` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct XmlReaderExt;

/// The state attached to every `XMLReader` object.
#[derive(Debug, Default)]
struct State {
    reader: XmlReader,
}

type StateRef = Rc<RefCell<State>>;

fn state_of(o: &Obj) -> Option<StateRef> {
    php_vm::object::internal::<StateRef, _>(o, |s| s.clone())
}

fn this_state(vm: &mut Vm, a: &Args) -> VmResult<StateRef> {
    let st = a.this.as_ref().and_then(state_of);
    st.ok_or_else(|| vm.throw("Error", "XMLReader object is not initialized"))
}

/// Throws a [`ReaderError`].
fn throw(vm: &mut Vm, e: &ReaderError) -> Throw {
    vm.throw(e.class.name(), &e.message)
}

/// Emits the warnings/notices recorded by a [`CollectingHost`].
fn flush_host(vm: &mut Vm, host: CollectingHost) -> VmResult<()> {
    for (is_warning, msg) in host.messages {
        if is_warning {
            vm.php_warning(&msg)?;
        } else {
            vm.php_notice(&msg)?;
        }
    }
    Ok(())
}

/// Runs `f` on the reader state inside [`php_ext_xml::libxml::run`]:
/// the VM is lent to the stream layer (stream-backed readers pull data
/// lazily; user entity loaders) and libxml2 diagnostics are reported.
fn with_reader<R>(
    vm: &mut Vm,
    a: &Args,
    st: &StateRef,
    f: impl FnOnce(&mut XmlReader) -> R,
) -> VmResult<R> {
    php_ext_xml::libxml::run(vm, a, || f(&mut st.borrow_mut().reader))
}

fn opt_bytes(a: &Args, n: usize) -> Option<Vec<u8>> {
    a.opt(n).map(|v| match v {
        Value::Str(s) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    })
}

fn str_value(bytes: Vec<u8>) -> Value {
    Value::Str(ZStr::from(bytes))
}

fn opt_str_value(bytes: Option<Vec<u8>>) -> Value {
    bytes.map_or(Value::Null, str_value)
}

fn prop_value(v: PropValue) -> Value {
    match v {
        PropValue::Int(i) => Value::Int(i),
        PropValue::Bool(b) => Value::Bool(b),
        PropValue::Str(s) => str_value(s),
    }
}

// ---------------------------------------------------------------------------
// Object handlers
// ---------------------------------------------------------------------------

#[allow(clippy::unnecessary_wraps)] // signature fixed by `ClassHooks::create`
fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    php_vm::object::set_internal::<StateRef>(o, Rc::new(RefCell::new(State::default())));
    Ok(())
}

fn virtual_prop(o: &Obj, name: &ZStr) -> Option<(StateRef, Property)> {
    let p = Property::from_name(std::str::from_utf8(name.as_bytes()).ok()?)?;
    Some((state_of(o)?, p))
}

fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    let Some((st, p)) = virtual_prop(o, name) else {
        return Ok(None);
    };
    let r = st.borrow().reader.read_property(p);
    match r {
        Ok(v) => Ok(Some(prop_value(v))),
        Err(e) => Err(throw(vm, &e)),
    }
}

fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, _v: &Value) -> VmResult<bool> {
    let Some((_, p)) = virtual_prop(o, name) else {
        return Ok(false);
    };
    let e = XmlReader::write_property_error(&o.class_name().to_string_lossy(), p);
    Err(throw(vm, &e))
}

fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    let Some((st, p)) = virtual_prop(o, name) else {
        return Ok(None);
    };
    let kind = if vm.property_context() == PropCtx::Exists {
        IssetKind::Exists
    } else if check_empty {
        IssetKind::NotEmpty
    } else {
        IssetKind::Isset
    };
    let r = st.borrow().reader.has_property(p, kind);
    r.map(Some).map_err(|e| throw(vm, &e))
}

fn unset_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<bool> {
    let Some((_, p)) = virtual_prop(o, name) else {
        return Ok(false);
    };
    let e = XmlReader::unset_property_error(&o.class_name().to_string_lossy(), p);
    Err(throw(vm, &e))
}

/// `"\0Class\0name"` / `"\0*\0name"` / `"name"`.
fn mangle(fl: u32, class: &ZStr, name: &ZStr) -> ZStr {
    let mut v = Vec::new();
    if fl & flags::PRIVATE != 0 {
        v.push(0);
        v.extend_from_slice(class.as_bytes());
        v.push(0);
    } else if fl & flags::PROTECTED != 0 {
        v.extend_from_slice(b"\0*\0");
    } else {
        return name.clone();
    }
    v.extend_from_slice(name.as_bytes());
    ZStr::from(v)
}

/// `zend_std_get_properties`: initialized declared properties (mangled
/// names) followed by dynamic ones.
fn std_properties(o: &Obj) -> HashTable {
    let cls = class_of(o).clone();
    let d = data(o);
    let mut t = HashTable::new();
    {
        let slots = d.slots.borrow();
        for pi in &cls.props {
            if pi.flags & flags::STATIC != 0 || pi.is_virtual() {
                continue;
            }
            if let Some(Some(v)) = slots.get(pi.slot as usize) {
                t.insert(
                    ArrayKey::Str(mangle(pi.flags, &pi.class, &pi.name)),
                    v.clone(),
                );
            }
        }
    }
    if let Some(dynp) = d.dynamic.borrow().as_deref() {
        for (k, v) in dynp {
            t.insert(k.clone(), v.clone());
        }
    }
    t
}

fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    let Some(st) = state_of(o) else {
        return Ok(None);
    };
    let mut t = std_properties(o);
    let (props, err) = st.borrow().reader.debug_properties();
    for (name, v) in props {
        t.insert(ArrayKey::Str(ZStr::from(name)), prop_value(v));
    }
    if let Some(e) = err {
        // PHP leaves the exception pending and still returns the table:
        // `var_dump()` prints it, then raises the parked exception.
        let pending = throw(vm, &e);
        if vm.pending_throw.is_none() {
            vm.pending_throw = Some(pending);
        }
    }
    Ok(Some(ZArray::from_table(t)))
}

fn hooks() -> ClassHooks {
    ClassHooks {
        create: Some(create),
        properties_for: Some(properties_for),
        read_property: Some(read_property),
        write_property: Some(write_property),
        has_property: Some(has_property),
        unset_property: Some(unset_property),
        ..ClassHooks::default()
    }
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

fn close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let mut s = st.borrow_mut();
    s.reader.close();
    Ok(Value::Bool(true))
}

fn get_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let name = a.str(0);
    let r = with_reader(vm, a, &st, |r| r.get_attribute(name.as_bytes()))?;
    r.map(opt_str_value).map_err(|e| throw(vm, &e))
}

fn get_attribute_no(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let index = a.int(0);
    with_reader(vm, a, &st, |r| r.get_attribute_no(index)).map(opt_str_value)
}

fn get_attribute_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let (name, ns) = (a.str(0), a.str(1));
    let r = with_reader(vm, a, &st, |r| {
        r.get_attribute_ns(name.as_bytes(), ns.as_bytes())
    })?;
    r.map(opt_str_value).map_err(|e| throw(vm, &e))
}

fn get_parser_property(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let p = a.int(0);
    let r = st.borrow().reader.get_parser_property(p);
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

fn set_parser_property(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let (p, v) = (a.int(0), a.bool(1));
    let r = with_reader(vm, a, &st, |r| r.set_parser_property(p, v))?;
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

fn is_valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::is_valid).map(Value::Bool)
}

fn lookup_namespace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let prefix = a.str(0);
    let r = with_reader(vm, a, &st, |r| r.lookup_namespace(prefix.as_bytes()))?;
    r.map(opt_str_value).map_err(|e| throw(vm, &e))
}

fn move_to_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let name = a.str(0);
    let r = with_reader(vm, a, &st, |r| r.move_to_attribute(name.as_bytes()))?;
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

fn move_to_attribute_no(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let index = a.int(0);
    with_reader(vm, a, &st, |r| r.move_to_attribute_no(index)).map(Value::Bool)
}

fn move_to_attribute_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let (name, ns) = (a.str(0), a.str(1));
    let r = with_reader(vm, a, &st, |r| {
        r.move_to_attribute_ns(name.as_bytes(), ns.as_bytes())
    })?;
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

fn move_to_element(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::move_to_element).map(Value::Bool)
}

fn move_to_first_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::move_to_first_attribute).map(Value::Bool)
}

fn move_to_next_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::move_to_next_attribute).map(Value::Bool)
}

fn read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let r = with_reader(vm, a, &st, XmlReader::read)?;
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

fn next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let name = opt_bytes(a, 0);
    let r = with_reader(vm, a, &st, |r| r.next(name.as_deref()))?;
    r.map(Value::Bool).map_err(|e| throw(vm, &e))
}

fn read_inner_xml(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::read_inner_xml).map(str_value)
}

fn read_outer_xml(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::read_outer_xml).map(str_value)
}

fn read_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    with_reader(vm, a, &st, XmlReader::read_string).map(str_value)
}

type SchemaFn = fn(&mut XmlReader, Option<&[u8]>, &mut CollectingHost) -> Result<bool, ReaderError>;

/// The shared body of `setSchema`/`setRelaxNGSchema*`.
fn schema_call(vm: &mut Vm, a: &Args, f: SchemaFn) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    let arg = opt_bytes(a, 0);
    let mut host = CollectingHost::default();
    let r = with_reader(vm, a, &st, |r| f(r, arg.as_deref(), &mut host))?;
    let v = r.map_err(|e| throw(vm, &e))?;
    flush_host(vm, host)?;
    Ok(Value::Bool(v))
}

fn set_schema(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    schema_call(vm, a, |r, s, h| r.set_schema(s, h))
}

fn set_relaxng_schema(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    schema_call(vm, a, |r, s, h| r.set_relaxng_schema(s, h))
}

fn set_relaxng_schema_source(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    schema_call(vm, a, |r, s, h| r.set_relaxng_schema_source(s, h))
}

const NO_DOM: &str = "XMLReader::expand() requires the DOM extension to be enabled";

fn expand(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_state(vm, a)?;
    if vm.find_class(b"DOMNode").is_none() {
        return Err(vm.throw("Error", NO_DOM));
    }
    // The copy goes into the base node's document (or none).
    let base = match a.opt(0) {
        Some(Value::Object(o)) => {
            let Some(b) = php_ext_dom::interop::import_node(&o) else {
                // `NODE_GET_OBJ`: an unconstructed DOM object.
                let class = php_vm::object::display_name(&o.class_name());
                vm.php_warning(&format!("Couldn't fetch {}", class.to_string_lossy()))?;
                return Ok(Value::Bool(false));
            };
            Some(b)
        }
        _ => None,
    };
    let doc = base
        .as_ref()
        .and_then(|b| b.document)
        .map_or(std::ptr::null_mut(), |d| d.as_ptr().cast());
    let mut host = CollectingHost::default();
    // SAFETY: `doc` is null or the live document of the base node, kept
    // alive by `base`'s keep-alive.
    let r = with_reader(vm, a, &st, |r| unsafe { r.expand_raw(doc, &mut host) })?;
    let node = r.map_err(|e| throw(vm, &e))?;
    flush_host(vm, host)?;
    match node {
        None => Ok(Value::Bool(false)),
        // SAFETY: a fresh detached copy whose ownership passes to the DOM
        // extension; its document (if any) is kept alive by `base`.
        Some(p) => unsafe {
            php_ext_dom::interop::node_object(vm, p.cast(), base.map(|b| b.keepalive))
        },
    }
}

/// Instantiates the class of a static constructor (calling its
/// constructor without arguments) and attaches `source`.
fn instantiate(vm: &mut Vm, class: &Rc<Class>, source: Source) -> VmResult<Value> {
    let o = vm.new_object_with_args(class, Vec::new())?;
    if let Some(st) = state_of(&o) {
        st.borrow_mut().reader.attach(source);
    }
    Ok(Value::Object(o))
}

fn base_class(vm: &mut Vm) -> VmResult<Rc<Class>> {
    match vm.find_class(b"XMLReader") {
        Some(c) => Ok(c),
        None => Err(vm.throw("Error", "Class \"XMLReader\" not found")),
    }
}

fn called_class(vm: &mut Vm, a: &Args) -> VmResult<Rc<Class>> {
    match a.called.clone() {
        Some(c) => Ok(c),
        None => base_class(vm),
    }
}

type InstanceLoad =
    fn(&mut XmlReader, &[u8], Option<&[u8]>, i64, &mut CollectingHost) -> Result<bool, ReaderError>;
type StaticLoad =
    fn(&[u8], Option<&[u8]>, i64, &mut CollectingHost) -> Result<Option<Source>, ReaderError>;

/// The shared body of `open()`/`XML()`: on an instance (re)loads it and
/// returns a `bool`; called statically returns a new `XMLReader` or
/// `false`.
fn legacy_load(
    vm: &mut Vm,
    a: &Args,
    on_obj: InstanceLoad,
    on_class: StaticLoad,
) -> VmResult<Value> {
    let arg = a.str(0);
    let encoding = opt_bytes(a, 1);
    let flags_ = a.int(2);
    let mut host = CollectingHost::default();
    if let Some(obj) = a.this.clone() {
        let Some(st) = state_of(&obj) else {
            return Ok(Value::Bool(false));
        };
        let r = with_reader(vm, a, &st, |r| {
            on_obj(r, arg.as_bytes(), encoding.as_deref(), flags_, &mut host)
        })?;
        let v = r.map_err(|e| throw(vm, &e))?;
        flush_host(vm, host)?;
        return Ok(Value::Bool(v));
    }
    let r = php_ext_xml::libxml::run(vm, a, || {
        on_class(arg.as_bytes(), encoding.as_deref(), flags_, &mut host)
    })?;
    let src = r.map_err(|e| throw(vm, &e))?;
    flush_host(vm, host)?;
    match src {
        Some(src) => {
            let cls = base_class(vm)?;
            instantiate(vm, &cls, src)
        }
        None => Ok(Value::Bool(false)),
    }
}

fn open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    legacy_load(
        vm,
        a,
        |r, s, e, f, h| r.open(s, e, f, h),
        |s, e, f, h| XmlReader::open_static(s, e, f, h),
    )
}

fn xml(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    legacy_load(
        vm,
        a,
        |r, s, e, f, h| r.xml(s, e, f, h),
        |s, e, f, h| XmlReader::xml_static(s, e, f, h),
    )
}

fn from_uri(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let uri = a.str(0);
    let encoding = opt_bytes(a, 1);
    let flags_ = a.int(2);
    let r = php_ext_xml::libxml::run(vm, a, || {
        XmlReader::from_uri(uri.as_bytes(), encoding.as_deref(), flags_)
    })?;
    let src = r.map_err(|e| throw(vm, &e))?;
    let cls = called_class(vm, a)?;
    instantiate(vm, &cls, src)
}

fn from_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let source = a.str(0);
    let encoding = opt_bytes(a, 1);
    let flags_ = a.int(2);
    let r = php_ext_xml::libxml::run(vm, a, || {
        XmlReader::from_string(source.as_bytes(), encoding.as_deref(), flags_)
    })?;
    let src = r.map_err(|e| throw(vm, &e))?;
    let cls = called_class(vm, a)?;
    instantiate(vm, &cls, src)
}

/// A PHP stream as libxml2's input: reads through the stream layer and
/// keeps the resource alive until the reader is freed (`Z_ADDREF`).
struct StreamInput {
    _res: Resource,
    cell: Rc<StreamCell>,
}

impl Read for StreamInput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(mut s) = self.cell.get() else {
            return Err(std::io::Error::other("stream is closed"));
        };
        s.read(buf)
            .map_err(|_| std::io::Error::other("stream read failed"))
    }
}

fn path_or_null(vm: &mut Vm, a: &Args, n: usize, name: &str) -> VmResult<Option<Vec<u8>>> {
    let v = opt_bytes(a, n);
    if v.as_ref().is_some_and(|s| s.contains(&0)) {
        return Err(vm.throw(
            "ValueError",
            &format!(
                "XMLReader::fromStream(): Argument #{} (${name}) must not contain any null bytes",
                n + 1
            ),
        ));
    }
    Ok(v)
}

fn from_stream(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let res = match a.at(0) {
        Value::Resource(r) => r,
        other => {
            let msg = format!(
                "XMLReader::fromStream(): Argument #1 ($stream) must be of type resource, {} given",
                php_ext_streams::bridge::given_type(&other)
            );
            return Err(vm.throw("TypeError", &msg));
        }
    };
    let encoding = path_or_null(vm, a, 1, "encoding")?;
    let flags_ = a.int(2);
    let uri = path_or_null(vm, a, 3, "documentUri")?;
    let cell = match cell_of(&res) {
        Some(c) if !res.is_closed() && !c.is_closed() => c,
        _ => {
            return Err(vm.throw(
                "TypeError",
                "XMLReader::fromStream(): supplied resource is not a valid stream resource",
            ))
        }
    };
    let input = StreamInput { _res: res, cell };
    let r = php_ext_xml::libxml::run(vm, a, || {
        XmlReader::from_stream(Box::new(input), encoding.as_deref(), flags_, uri.as_deref())
    })?;
    let src = r.map_err(|e| throw(vm, &e))?;
    let cls = called_class(vm, a)?;
    instantiate(vm, &cls, src)
}

impl Extension for XmlReaderExt {
    fn name(&self) -> &'static str {
        "xmlreader"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        let table = php_xmlreader::class::class_table();
        let pb = acc::PUBLIC;
        let st = acc::PUBLIC | acc::STATIC | acc::STATIC_KEEPS_THIS;
        let mut cb = ClassBuilder::new("XMLReader")
            .flags(class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE);
        for c in table.constants {
            cb = cb.constant(c.name, Value::Int(c.value));
        }
        // The virtual properties are declared virtual (no backing store) for
        // `property_exists()` and reflection; every access goes through
        // the hooks.
        for p in &table.properties {
            cb = cb.prop(p.name, acc::PUBLIC | acc::VIRTUAL, Some(p.type_), None);
        }
        cb.method("close(): true", pb, close)
            .method("getAttribute(string $name): ?string", pb, get_attribute)
            .method("getAttributeNo(int $index): ?string", pb, get_attribute_no)
            .method("getAttributeNs(string $name, string $namespace): ?string", pb, get_attribute_ns)
            .method("getParserProperty(int $property): bool", pb, get_parser_property)
            .method("isValid(): bool", pb, is_valid)
            .method("lookupNamespace(string $prefix): ?string", pb, lookup_namespace)
            .method("moveToAttribute(string $name): bool", pb, move_to_attribute)
            .method("moveToAttributeNo(int $index): bool", pb, move_to_attribute_no)
            .method("moveToAttributeNs(string $name, string $namespace): bool", pb, move_to_attribute_ns)
            .method("moveToElement(): bool", pb, move_to_element)
            .method("moveToFirstAttribute(): bool", pb, move_to_first_attribute)
            .method("moveToNextAttribute(): bool", pb, move_to_next_attribute)
            .method("read(): bool", pb, read)
            .method("next(?string $name = null): bool", pb, next)
            .method("open(string $uri, ?string $encoding = null, int $flags = 0)", st, open)
            .method("fromUri(string $uri, ?string $encoding = null, int $flags = 0): static", st, from_uri)
            .method(
                "fromStream($stream, ?string $encoding = null, int $flags = 0, ?string $documentUri = null): static",
                st,
                from_stream,
            )
            .method("readInnerXml(): string", pb, read_inner_xml)
            .method("readOuterXml(): string", pb, read_outer_xml)
            .method("readString(): string", pb, read_string)
            .method("setSchema(?string $filename): bool", pb, set_schema)
            .method("setParserProperty(int $property, bool $value): bool", pb, set_parser_property)
            .method("setRelaxNGSchema(?string $filename): bool", pb, set_relaxng_schema)
            .method("setRelaxNGSchemaSource(?string $source): bool", pb, set_relaxng_schema_source)
            .method("XML(string $source, ?string $encoding = null, int $flags = 0)", st, xml)
            .method("fromString(string $source, ?string $encoding = null, int $flags = 0): static", st, from_string)
            .method("expand(?DOMNode $baseNode = null): DOMNode|false", pb, expand)
            .hooks(hooks())
            .register(m.vm());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mangling() {
        let c = ZStr::from("C");
        let n = ZStr::from("p");
        assert_eq!(mangle(flags::PUBLIC, &c, &n).as_bytes(), b"p");
        assert_eq!(mangle(flags::PROTECTED, &c, &n).as_bytes(), b"\0*\0p");
        assert_eq!(mangle(flags::PRIVATE, &c, &n).as_bytes(), b"\0C\0p");
    }

    #[test]
    fn prop_values() {
        assert!(matches!(prop_value(PropValue::Int(3)), Value::Int(3)));
        assert!(matches!(
            prop_value(PropValue::Bool(true)),
            Value::Bool(true)
        ));
        assert!(matches!(opt_str_value(None), Value::Null));
        assert!(matches!(opt_str_value(Some(b"x".to_vec())), Value::Str(_)));
    }
}

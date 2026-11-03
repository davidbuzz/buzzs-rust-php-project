//! A miniature VM for replaying `ext/pdo/tests/*.phpt` over the PDO core:
//! a class table with closures as constructors/methods, objects with
//! property tables, `var_dump` output, diagnostics in CLI format and an
//! EXPECTF matcher.

#![allow(dead_code, clippy::missing_panics_doc, clippy::type_complexity)]

use php_pdo::host::{ClassInfo, PdoException, PdoHost};
use php_pdo::{ConnectArgs, ParamId, Pdo, PdoRow, PdoStatement, PersistentList};
use php_runtime::{
    ArrayKey, CastTarget, ErrorLevel, Obj, ObjectDump, ObjectKind, PhpObject, PropertyPurpose,
    PropertySlot, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

thread_local! {
    static NEXT_HANDLE: Cell<u32> = const { Cell::new(1) };
}

/// The kind of native state an object carries.
#[derive(Clone)]
pub enum Kind {
    Plain,
    Pdo(Pdo),
    Stmt(PdoStatement),
    Row(PdoRow),
}

/// A property slot.
#[derive(Clone)]
pub struct Prop {
    pub name: ZStr,
    pub class: ZStr,
    pub vis: Vis,
    pub ty: Option<&'static str>,
    /// `None`: uninitialized typed property.
    pub value: Option<Value>,
    pub declared: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vis {
    Public,
    Protected,
    Private,
}

impl Prop {
    fn key(&self) -> ZStr {
        match self.vis {
            Vis::Public => self.name.clone(),
            Vis::Protected => {
                let mut k = b"\0*\0".to_vec();
                k.extend_from_slice(self.name.as_bytes());
                ZStr::from(k)
            }
            Vis::Private => {
                let mut k = vec![0];
                k.extend_from_slice(self.class.as_bytes());
                k.push(0);
                k.extend_from_slice(self.name.as_bytes());
                ZStr::from(k)
            }
        }
    }
}

/// A test object.
pub struct TestObj {
    handle: u32,
    pub class: ZStr,
    pub props: RefCell<Vec<Prop>>,
    pub kind: RefCell<Kind>,
}

impl PhpObject for TestObj {
    fn handle(&self) -> u32 {
        self.handle
    }
    fn class_name(&self) -> ZStr {
        self.class.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub type Method = Rc<dyn Fn(&mut TestHost, &Value, &[Value]) -> RtResult<Value>>;
pub type Function = Rc<dyn Fn(&mut TestHost, &[Value]) -> RtResult<Value>>;
pub type Ctor = Rc<dyn Fn(&mut TestHost, &Value, Option<&ZArray>) -> RtResult<()>>;
pub type Setter = Rc<dyn Fn(&mut TestHost, &Value, &ZStr, Value) -> RtResult<()>>;

/// A property declaration.
#[derive(Clone)]
pub struct PropDecl {
    pub name: &'static str,
    pub vis: Vis,
    pub ty: Option<&'static str>,
    pub default: Option<Value>,
}

/// A class.
#[derive(Clone, Default)]
pub struct Class {
    pub name: String,
    pub parent: Option<String>,
    pub is_abstract: bool,
    pub props: Vec<PropDecl>,
    pub ctor: Option<Ctor>,
    pub ctor_public: bool,
    pub set: Option<Setter>,
    pub to_string: Option<Method>,
    pub methods: HashMap<String, (Method, bool, Vis)>,
    pub dynamic_ok: bool,
    pub unserialize: Option<Rc<dyn Fn(&mut TestHost, &Value, &[u8]) -> RtResult<()>>>,
}

impl Class {
    pub fn new(name: &str) -> Self {
        Class {
            name: name.to_string(),
            ctor_public: true,
            ..Default::default()
        }
    }
    pub fn extends(mut self, parent: &str) -> Self {
        self.parent = Some(parent.to_string());
        self
    }
    pub fn prop(mut self, name: &'static str) -> Self {
        self.props.push(PropDecl {
            name,
            vis: Vis::Public,
            ty: None,
            default: Some(Value::Null),
        });
        self
    }
    pub fn prop_vis(mut self, name: &'static str, vis: Vis) -> Self {
        self.props.push(PropDecl {
            name,
            vis,
            ty: None,
            default: Some(Value::Null),
        });
        self
    }
    pub fn prop_default(mut self, name: &'static str, vis: Vis, v: Value) -> Self {
        self.props.push(PropDecl {
            name,
            vis,
            ty: None,
            default: Some(v),
        });
        self
    }
    pub fn typed(mut self, name: &'static str, ty: &'static str) -> Self {
        self.props.push(PropDecl {
            name,
            vis: Vis::Public,
            ty: Some(ty),
            default: None,
        });
        self
    }
    pub fn ctor(
        mut self,
        f: impl Fn(&mut TestHost, &Value, Option<&ZArray>) -> RtResult<()> + 'static,
    ) -> Self {
        self.ctor = Some(Rc::new(f));
        self
    }
    pub fn private_ctor(mut self) -> Self {
        self.ctor_public = false;
        self
    }
    pub fn setter(
        mut self,
        f: impl Fn(&mut TestHost, &Value, &ZStr, Value) -> RtResult<()> + 'static,
    ) -> Self {
        self.set = Some(Rc::new(f));
        self
    }
    pub fn method(
        mut self,
        name: &str,
        is_static: bool,
        vis: Vis,
        f: impl Fn(&mut TestHost, &Value, &[Value]) -> RtResult<Value> + 'static,
    ) -> Self {
        self.methods
            .insert(name.to_ascii_lowercase(), (Rc::new(f), is_static, vis));
        self
    }
    pub fn dynamic(mut self) -> Self {
        self.dynamic_ok = true;
        self
    }
}

/// The test VM.
pub struct TestHost {
    pub out: Vec<u8>,
    pub func: String,
    pub classes: HashMap<String, Class>,
    pub functions: HashMap<String, Function>,
    pub autoload: Option<Rc<dyn Fn(&mut TestHost, &ZStr)>>,
    pub persistent: PersistentList,
    pub ini: HashMap<Vec<u8>, Vec<u8>>,
    /// `set_error_handler()`: receives the message.
    pub error_handler: Option<Rc<dyn Fn(&mut TestHost, &str) -> RtResult<()>>>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

fn level_label(level: ErrorLevel) -> &'static str {
    match level {
        ErrorLevel::Warning => "Warning",
        ErrorLevel::Notice => "Notice",
        ErrorLevel::Deprecated => "Deprecated",
        _ => level.label(),
    }
}

impl TestHost {
    pub fn new() -> Self {
        let mut h = TestHost {
            out: Vec::new(),
            func: String::new(),
            classes: HashMap::new(),
            functions: HashMap::new(),
            autoload: None,
            persistent: PersistentList::new(),
            ini: HashMap::new(),
            error_handler: None,
        };
        h.add_class(Class::new("stdClass").dynamic());
        h.add_class(Class::new("PDO"));
        h.add_class(Class::new("Pdo\\Sqlite").extends("PDO"));
        h.add_class(Class::new("PDOStatement").typed("queryString", "string"));
        h.add_class(Class::new("PDORow").typed("queryString", "string"));
        h.add_class(Class::new("Exception").dynamic());
        h.add_class(
            Class::new("RuntimeException")
                .extends("Exception")
                .dynamic(),
        );
        h.add_class(
            Class::new("PDOException")
                .extends("RuntimeException")
                .dynamic(),
        );
        h
    }

    pub fn add_class(&mut self, c: Class) {
        self.classes.insert(c.name.to_ascii_lowercase(), c);
    }

    pub fn add_function(
        &mut self,
        name: &str,
        f: impl Fn(&mut TestHost, &[Value]) -> RtResult<Value> + 'static,
    ) {
        self.functions.insert(name.to_ascii_lowercase(), Rc::new(f));
    }

    pub fn class(&self, name: &str) -> Option<&Class> {
        self.classes.get(&name.to_ascii_lowercase())
    }

    fn chain(&self, name: &str) -> Vec<Class> {
        let mut out = Vec::new();
        let mut cur = self.class(name).cloned();
        while let Some(c) = cur {
            cur = c.parent.as_deref().and_then(|p| self.class(p)).cloned();
            out.push(c);
        }
        out
    }

    pub fn is_a(&self, class: &str, parent: &str) -> bool {
        self.chain(class)
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(parent))
    }

    pub fn echo(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }

    pub fn obj(v: &Value) -> Rc<dyn PhpObject> {
        match v.deref() {
            Value::Object(o) => o.rc().clone(),
            other => panic!("not an object: {other:?}"),
        }
    }

    pub fn with_obj<R>(v: &Value, f: impl FnOnce(&TestObj) -> R) -> R {
        let rc = Self::obj(v);
        let o = rc.as_any().downcast_ref::<TestObj>().expect("test object");
        f(o)
    }

    pub fn new_object(&mut self, class: &str) -> Value {
        let chain = self.chain(class);
        let canonical = chain
            .first()
            .map_or_else(|| class.to_string(), |c| c.name.clone());
        let mut props = Vec::new();
        for c in chain.iter().rev() {
            for p in &c.props {
                if let Some(existing) = props.iter_mut().find(|q: &&mut Prop| {
                    q.name.as_bytes() == p.name.as_bytes() && p.vis != Vis::Private
                }) {
                    existing.value = p.default.clone();
                    continue;
                }
                props.push(Prop {
                    name: ZStr::from(p.name),
                    class: ZStr::from(c.name.as_str()),
                    vis: p.vis,
                    ty: p.ty,
                    value: p.default.clone(),
                    declared: true,
                });
            }
        }
        let handle = NEXT_HANDLE.with(|h| {
            let v = h.get();
            h.set(v + 1);
            v
        });
        let obj = Obj::new(TestObj {
            handle,
            class: ZStr::from(canonical.as_str()),
            props: RefCell::new(props),
            kind: RefCell::new(Kind::Plain),
        });
        let v = Value::Object(obj.clone());
        if self.is_a(&canonical, "PDOStatement") {
            let st = PdoStatement::new();
            st.attach_object(&obj);
            Self::with_obj(&v, |o| *o.kind.borrow_mut() = Kind::Stmt(st));
        } else if self.is_a(&canonical, "PDO") {
            let p = Pdo::new();
            p.attach_object(&obj);
            Self::with_obj(&v, |o| *o.kind.borrow_mut() = Kind::Pdo(p));
        }
        v
    }

    pub fn get_prop(v: &Value, name: &str) -> Option<Value> {
        Self::with_obj(v, |o| {
            o.props
                .borrow()
                .iter()
                .find(|p| p.name.as_bytes() == name.as_bytes())
                .and_then(|p| p.value.clone())
        })
    }

    pub fn set_prop(v: &Value, name: &str, value: Value) {
        Self::with_obj(v, |o| {
            let mut props = o.props.borrow_mut();
            if let Some(p) = props
                .iter_mut()
                .find(|p| p.name.as_bytes() == name.as_bytes())
            {
                p.value = Some(value);
            } else {
                props.push(Prop {
                    name: ZStr::from(name),
                    class: o.class.clone(),
                    vis: Vis::Public,
                    ty: None,
                    value: Some(value),
                    declared: false,
                });
            }
        });
    }

    pub fn pdo(v: &Value) -> Pdo {
        Self::with_obj(v, |o| match &*o.kind.borrow() {
            Kind::Pdo(p) => p.clone(),
            _ => panic!("not a PDO object"),
        })
    }

    pub fn stmt(v: &Value) -> PdoStatement {
        Self::with_obj(v, |o| match &*o.kind.borrow() {
            Kind::Stmt(s) => s.clone(),
            _ => panic!("not a PDOStatement object"),
        })
    }

    pub fn row(v: &Value) -> Option<PdoRow> {
        Self::with_obj(v, |o| match &*o.kind.borrow() {
            Kind::Row(r) => Some(r.clone()),
            _ => None,
        })
    }

    pub fn dump(&mut self, v: &Value) {
        let mut out = Vec::new();
        if let Err(e) = php_runtime::dump::var_dump(&mut out, v, self) {
            panic!("var_dump failed: {e:?}");
        }
        self.out.extend_from_slice(&out);
    }

    pub fn export(&mut self, v: &Value) -> String {
        let r = php_runtime::dump::var_export(v, self).expect("var_export");
        String::from_utf8_lossy(&r).into_owned()
    }

    pub fn print_r(&mut self, v: &Value) {
        let r = php_runtime::dump::print_r(v, self).expect("print_r");
        self.out.extend_from_slice(&r);
    }

    /// `$e::class . ': ' . $e->getMessage()`.
    pub fn describe_error(&mut self, e: &RuntimeError) -> String {
        match e {
            RuntimeError::Thrown(v) => {
                let class = Self::obj(v).class_name().to_string_lossy();
                let msg = Self::get_prop(v, "message")
                    .and_then(|m| m.as_str().map(ZStr::to_string_lossy))
                    .unwrap_or_default();
                format!("{class}: {msg}")
            }
            other => format!(
                "{}: {}",
                other.exception_class().unwrap_or_default(),
                other.message().unwrap_or_default()
            ),
        }
    }

    pub fn error_class(e: &RuntimeError) -> String {
        match e {
            RuntimeError::Thrown(v) => Self::obj(v).class_name().to_string_lossy(),
            other => other.exception_class().unwrap_or_default(),
        }
    }

    pub fn error_message(e: &RuntimeError) -> String {
        match e {
            RuntimeError::Thrown(v) => Self::get_prop(v, "message")
                .and_then(|m| m.as_str().map(ZStr::to_string_lossy))
                .unwrap_or_default(),
            other => other.message().unwrap_or_default().to_string(),
        }
    }

    /// The exception object's property (`code`, `errorInfo`, ...).
    pub fn error_prop(e: &RuntimeError, name: &str) -> Option<Value> {
        match e {
            RuntimeError::Thrown(v) => Self::get_prop(v, name),
            _ => None,
        }
    }

    /// Prints an uncaught exception like the CLI does, as thrown in
    /// `script`.
    pub fn uncaught_in(&mut self, e: &RuntimeError, script: &str) {
        let d = self.describe_error(e);
        let (class, msg) = d.split_once(": ").unwrap_or((&d, ""));
        let s = format!(
            "\nFatal error: Uncaught {class}: {msg} in /t/{script}:1\nStack trace:\n#0 {{main}}\n  thrown in /t/{script} on line 1\n"
        );
        self.echo(&s);
    }

    /// Prints an uncaught exception like the CLI does.
    pub fn uncaught(&mut self, e: &RuntimeError) {
        let d = self.describe_error(e);
        let (class, msg) = d.split_once(": ").unwrap_or((&d, ""));
        let s = format!(
            "\nFatal error: Uncaught {class}: {msg} in /test.php:1\nStack trace:\n#0 {{main}}\n  thrown in /test.php on line 1\n"
        );
        self.echo(&s);
    }

    fn find_method(&self, class: &str, name: &str) -> Option<(Method, bool, Vis, String)> {
        for c in self.chain(class) {
            if let Some((m, s, v)) = c.methods.get(&name.to_ascii_lowercase()) {
                return Some((m.clone(), *s, *v, c.name.clone()));
            }
        }
        None
    }

    fn find_ctor(&self, class: &str) -> Option<(Ctor, bool)> {
        self.chain(class)
            .into_iter()
            .find_map(|c| c.ctor.clone().map(|f| (f, c.ctor_public)))
    }

    fn coerce(&mut self, class: &str, p: &Prop, value: Value) -> RtResult<Value> {
        let Some(ty) = p.ty else {
            return Ok(value);
        };
        let v = value.deref();
        let ok = match (ty.trim_start_matches('?'), &v) {
            (_, Value::Null) if ty.starts_with('?') || ty == "mixed" => return Ok(v),
            ("mixed", _) => return Ok(v),
            ("string", Value::Str(_)) | ("int", Value::Int(_)) => return Ok(v),
            ("string", Value::Int(_) | Value::Float(_)) => Some(Value::Str(v.to_zstr(self)?)),
            ("int", Value::Str(s)) => {
                match php_runtime::numeric::is_numeric_string(s.as_bytes(), false) {
                    Some(php_runtime::numeric::Number::Int(i)) => Some(Value::Int(i)),
                    _ => None,
                }
            }
            _ => None,
        };
        ok.ok_or_else(|| {
            RuntimeError::TypeError(format!(
                "Cannot assign {} to property {}::${} of type {ty}",
                v.type_name(),
                class,
                p.name
            ))
        })
    }

    fn can_access(&self, p: &Prop, scope: Option<&ZStr>) -> bool {
        match p.vis {
            Vis::Public => true,
            Vis::Private => {
                scope.is_some_and(|s| s.as_bytes().eq_ignore_ascii_case(p.class.as_bytes()))
            }
            Vis::Protected => scope.is_some_and(|s| {
                let s = s.to_string_lossy();
                let c = p.class.to_string_lossy();
                self.is_a(&s, &c) || self.is_a(&c, &s)
            }),
        }
    }
}

impl RuntimeContext for TestHost {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        let s = format!(
            "\n{}: {} in /test.php on line 1\n",
            level_label(level),
            message
        );
        self.echo(&s);
        Ok(())
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        if matches!(target, CastTarget::String) {
            let class = obj.class_name().to_string_lossy();
            if let Some((m, _, _, _)) = self.find_method(&class, "__tostring") {
                let v = Value::Object(obj.clone());
                return m(self, &v, &[]).map(Some);
            }
            return Err(RuntimeError::Error(format!(
                "Object of class {class} could not be converted to string"
            )));
        }
        Ok(match target {
            CastTarget::Bool => Some(Value::Bool(true)),
            _ => None,
        })
    }

    fn object_properties(
        &mut self,
        obj: &Obj,
        purpose: PropertyPurpose,
    ) -> RtResult<Option<ZArray>> {
        let Some(o) = obj.downcast_ref::<TestObj>() else {
            return Ok(None);
        };
        let kind = o.kind.borrow().clone();
        if let (Kind::Row(r), PropertyPurpose::Debug) = (&kind, purpose) {
            return r.debug_properties(self).map(Some);
        }
        let mut t = ZArray::new();
        for p in o.props.borrow().iter() {
            if let Some(v) = &p.value {
                t.make_mut().insert(ArrayKey::Str(p.key()), v.clone());
            }
        }
        Ok(Some(t))
    }

    fn object_dump(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<ObjectDump> {
        let Some(o) = obj.downcast_ref::<TestObj>() else {
            return Ok(ObjectDump {
                kind: ObjectKind::Regular { lazy_prefix: "" },
                properties: None,
            });
        };
        let kind = o.kind.borrow().clone();
        let is_std = o.class.as_bytes() == b"stdClass";
        let objkind = if is_std {
            ObjectKind::StdClass
        } else {
            ObjectKind::Regular { lazy_prefix: "" }
        };
        if let (Kind::Row(r), PropertyPurpose::Debug) = (&kind, purpose) {
            let t = r.debug_properties(self)?;
            return Ok(ObjectDump {
                kind: objkind,
                properties: Some(
                    t.iter()
                        .map(|(k, v)| (k.clone(), PropertySlot::Value(v.clone())))
                        .collect(),
                ),
            });
        }
        let props = o
            .props
            .borrow()
            .iter()
            .map(|p| {
                (
                    ArrayKey::Str(p.key()),
                    match &p.value {
                        Some(v) => PropertySlot::Value(v.clone()),
                        None => PropertySlot::Uninitialized(ZStr::from(p.ty.unwrap_or("mixed"))),
                    },
                )
            })
            .collect();
        Ok(ObjectDump {
            kind: objkind,
            properties: Some(props),
        })
    }

    fn property_read(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        let v = Value::Object(obj.clone());
        Ok(Self::get_prop(&v, &name.to_string_lossy()))
    }

    fn property_write(&mut self, obj: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
        let v = Value::Object(obj.clone());
        Self::set_prop(&v, &name.to_string_lossy(), value);
        Ok(())
    }

    fn create_std_object(&mut self, props: ZArray) -> RtResult<Value> {
        let v = self.new_object("stdClass");
        for (k, val) in props.iter() {
            Self::set_prop(&v, &k.to_zstr().to_string_lossy(), val.clone());
        }
        Ok(v)
    }
}

impl PdoHost for TestHost {
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        match callable.deref() {
            Value::Str(name) => {
                let f = self
                    .functions
                    .get(&name.to_string_lossy().to_ascii_lowercase())
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Error(format!("Call to undefined function {name}()"))
                    })?;
                f(self, args)
            }
            Value::Array(a) => {
                let target = a.get_int(0).cloned().unwrap_or(Value::Null);
                let method = a
                    .get_int(1)
                    .and_then(|m| m.as_str().cloned())
                    .unwrap_or_else(ZStr::empty);
                let class = match &target {
                    Value::Object(o) => o.class_name().to_string_lossy(),
                    Value::Str(s) => s.to_string_lossy(),
                    _ => String::new(),
                };
                let (m, _, _, _) = self
                    .find_method(&class, &method.to_string_lossy())
                    .ok_or_else(|| RuntimeError::Error("undefined method".into()))?;
                m(self, &target, args)
            }
            _ => Err(RuntimeError::Error("not callable".into())),
        }
    }

    fn check_callable(&mut self, callable: &Value) -> Result<(), String> {
        match callable.deref() {
            Value::Str(name) => {
                let n = name.to_string_lossy();
                if self.functions.contains_key(&n.to_ascii_lowercase()) {
                    Ok(())
                } else {
                    Err(format!(
                        "function \"{n}\" not found or invalid function name"
                    ))
                }
            }
            Value::Array(a) if a.len() == 2 => {
                let target = a.get_int(0).cloned().unwrap_or(Value::Null);
                let method = a
                    .get_int(1)
                    .and_then(|m| m.as_str().cloned())
                    .unwrap_or_else(ZStr::empty);
                let (class, is_obj) = match &target {
                    Value::Object(o) => (o.class_name().to_string_lossy(), true),
                    Value::Str(s) => (s.to_string_lossy(), false),
                    _ => {
                        return Err("first array member is not a valid class name or object".into())
                    }
                };
                if class.eq_ignore_ascii_case("self") {
                    return Err("cannot access \"self\" when no class scope is active".into());
                }
                let Some(c) = self.class(&class).map(|c| c.name.clone()) else {
                    return Err(format!("class \"{class}\" not found"));
                };
                let m = method.to_string_lossy();
                match self.find_method(&c, &m) {
                    None => Err(format!("class {c} does not have a method \"{m}\"")),
                    Some((_, is_static, vis, owner)) => {
                        if vis == Vis::Private {
                            return Err(format!("cannot access private method {owner}::{m}()"));
                        }
                        if !is_static && !is_obj {
                            return Err(format!(
                                "non-static method {owner}::{m}() cannot be called statically"
                            ));
                        }
                        Ok(())
                    }
                }
            }
            _ => Err("no array or string given".into()),
        }
    }

    fn docref(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.docref_ex(level, message, false)
    }

    fn docref_ex(&mut self, level: ErrorLevel, message: &str, pending: bool) -> RtResult<()> {
        let msg = if self.func.is_empty() {
            message.to_string()
        } else {
            format!("{}(): {message}", self.func)
        };
        if let Some(handler) = self.error_handler.clone() {
            if pending {
                return Ok(());
            }
            return handler(self, &msg);
        }
        self.diagnostic(level, &msg)
    }

    fn active_function_name(&self) -> String {
        self.func.clone()
    }

    fn lookup_class(&mut self, name: &ZStr) -> Option<ClassInfo> {
        let n = name.to_string_lossy();
        if self.class(&n).is_none() {
            if let Some(a) = self.autoload.clone() {
                a(self, name);
            }
        }
        let c = self.class(&n)?.clone();
        let ctor = self.find_ctor(&c.name);
        Some(ClassInfo {
            name: ZStr::from(c.name.as_str()),
            has_constructor: ctor.is_some(),
            constructor_is_public: ctor.is_some_and(|(_, p)| p),
            can_unserialize: self.chain(&c.name).iter().any(|c| c.unserialize.is_some()),
        })
    }

    fn instance_of(&mut self, class: &ZStr, parent: &str) -> bool {
        self.is_a(&class.to_string_lossy(), parent)
    }

    fn instantiate(&mut self, class: &ZStr) -> RtResult<Value> {
        let n = class.to_string_lossy();
        if let Some(c) = self.class(&n) {
            if c.is_abstract {
                return Err(RuntimeError::Error(format!(
                    "Cannot instantiate abstract class {}",
                    c.name
                )));
            }
        }
        Ok(self.new_object(&n))
    }

    fn call_constructor(&mut self, obj: &Value, args: Option<&ZArray>) -> RtResult<()> {
        let class = Self::obj(obj).class_name().to_string_lossy();
        if let Some((f, _)) = self.find_ctor(&class) {
            f(self, obj, args)?;
        }
        Ok(())
    }

    fn update_property(
        &mut self,
        obj: &Value,
        scope: Option<&ZStr>,
        name: &ZStr,
        value: Value,
    ) -> RtResult<()> {
        let class = Self::obj(obj).class_name().to_string_lossy();
        let setter = self.chain(&class).into_iter().find_map(|c| c.set);
        let slot = Self::with_obj(obj, |o| {
            o.props
                .borrow()
                .iter()
                .position(|p| p.name.as_bytes() == name.as_bytes())
                .map(|i| o.props.borrow()[i].clone())
        });
        // a parent's private property is invisible from other scopes
        let slot = slot.filter(|p| {
            !(p.vis == Vis::Private
                && !p.class.as_bytes().eq_ignore_ascii_case(class.as_bytes())
                && !self.can_access(p, scope))
        });
        match slot {
            Some(p) => {
                if !self.can_access(&p, scope) {
                    if let Some(s) = setter {
                        return s(self, obj, name, value);
                    }
                    let vis = if p.vis == Vis::Private {
                        "private"
                    } else {
                        "protected"
                    };
                    return Err(RuntimeError::Error(format!(
                        "Cannot access {vis} property {class}::${name}"
                    )));
                }
                let v = self.coerce(&class, &p, value)?;
                Self::with_obj(obj, |o| {
                    if let Some(q) = o
                        .props
                        .borrow_mut()
                        .iter_mut()
                        .find(|q| q.name.as_bytes() == name.as_bytes())
                    {
                        q.value = Some(v);
                    }
                });
                Ok(())
            }
            None => {
                if let Some(s) = setter {
                    return s(self, obj, name, value);
                }
                let dynamic_ok = self.chain(&class).iter().any(|c| c.dynamic_ok);
                if !dynamic_ok {
                    self.diagnostic(
                        ErrorLevel::Deprecated,
                        &format!("Creation of dynamic property {class}::${name} is deprecated"),
                    )?;
                }
                Self::with_obj(obj, |o| {
                    o.props.borrow_mut().push(Prop {
                        name: name.clone(),
                        class: o.class.clone(),
                        vis: Vis::Public,
                        ty: None,
                        value: Some(value.deref()),
                        declared: false,
                    });
                });
                Ok(())
            }
        }
    }

    fn unserialize_class(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        let n = class.to_string_lossy();
        let f = self.chain(&n).into_iter().find_map(|c| c.unserialize);
        let obj = self.new_object(&n);
        if let Some(f) = f {
            f(self, &obj, data)?;
        }
        Ok(obj)
    }

    fn instantiate_statement(&mut self, class: &ZStr) -> RtResult<(Value, PdoStatement)> {
        let v = self.instantiate(class)?;
        let st = Self::stmt(&v);
        Ok((v, st))
    }

    fn instantiate_pdo(&mut self, class: &ZStr) -> RtResult<(Value, Pdo)> {
        let v = self.instantiate(class)?;
        let p = Self::pdo(&v);
        Ok((v, p))
    }

    fn std_write_property(&mut self, obj: &Value, name: &ZStr, value: Value) -> RtResult<()> {
        Self::set_prop(obj, &name.to_string_lossy(), value);
        Ok(())
    }

    fn create_row(&mut self, row: PdoRow) -> RtResult<Value> {
        let v = self.new_object("PDORow");
        Self::with_obj(&v, |o| {
            o.props.borrow_mut().clear();
            *o.kind.borrow_mut() = Kind::Row(row);
        });
        Ok(v)
    }

    fn new_pdo_exception(&mut self, e: PdoException) -> RuntimeError {
        let v = self.new_object("PDOException");
        Self::set_prop(&v, "message", Value::from(e.message.as_str()));
        Self::set_prop(&v, "code", e.code);
        Self::set_prop(
            &v,
            "errorInfo",
            e.error_info.map_or(Value::Null, Value::Array),
        );
        RuntimeError::Thrown(v)
    }

    fn chain_exception(&mut self, exception: RuntimeError, previous: RuntimeError) -> RuntimeError {
        if let RuntimeError::Thrown(v) = &exception {
            let prev = match previous {
                RuntimeError::Thrown(p) => p,
                other => {
                    let o = self.new_object("Exception");
                    Self::set_prop(
                        &o,
                        "message",
                        Value::from(other.message().unwrap_or_default()),
                    );
                    o
                }
            };
            Self::set_prop(v, "previous", prev);
        }
        exception
    }

    fn read_stream(&mut self, value: &Value) -> Option<Vec<u8>> {
        match value.deref() {
            Value::Resource(r) => r
                .data()
                .as_ref()
                .and_then(|d| d.downcast_ref::<Vec<u8>>().cloned()),
            _ => None,
        }
    }

    fn memory_stream(&mut self, data: ZStr) -> RtResult<Value> {
        Ok(Value::Resource(php_runtime::Resource::new(
            "stream",
            Some(Box::new(data.as_bytes().to_vec())),
        )))
    }

    fn write_output(&mut self, bytes: &[u8]) -> RtResult<()> {
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    fn cfg_get_string(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        self.ini.get(name).cloned()
    }

    fn persistent_list(&mut self) -> &mut PersistentList {
        &mut self.persistent
    }
}

// ---------------------------------------------------------------------
// Convenience wrappers (`$db->...`, `$stmt->...`)
// ---------------------------------------------------------------------

pub fn s(v: &str) -> Value {
    Value::from(v)
}

pub fn i(v: i64) -> Value {
    Value::Int(v)
}

pub fn arr(vals: Vec<Value>) -> ZArray {
    ZArray::from_values(vals)
}

pub fn assoc(pairs: Vec<(&str, Value)>) -> ZArray {
    ZArray::from_pairs(
        pairs
            .into_iter()
            .map(|(k, v)| (ArrayKey::from_bytes(k.as_bytes()), v)),
    )
}

pub fn iassoc(pairs: Vec<(i64, Value)>) -> ZArray {
    ZArray::from_pairs(pairs.into_iter().map(|(k, v)| (ArrayKey::Int(k), v)))
}

pub fn z(v: &str) -> ZStr {
    ZStr::from(v)
}

impl TestHost {
    fn enter(&mut self, f: &str) -> String {
        std::mem::replace(&mut self.func, f.to_string())
    }

    /// `new $class($dsn, $user, $pass, $options)`.
    pub fn new_pdo(&mut self, class: &str, dsn: &str, options: Option<ZArray>) -> RtResult<Value> {
        let v = self.new_object(class);
        let p = Self::pdo(&v);
        let old = self.enter("PDO::__construct");
        let cls = Self::obj(&v).class_name();
        let r = p.construct(
            self,
            &cls,
            ConnectArgs {
                dsn: dsn.as_bytes(),
                username: None,
                password: None,
                options: options.as_ref(),
            },
        );
        self.func = old;
        r.map(|()| v)
    }

    /// `$class::connect($dsn, ...)`.
    pub fn connect(&mut self, class: &str, dsn: &str, options: Option<ZArray>) -> RtResult<Value> {
        let old = self.enter("PDO::connect");
        let r = Pdo::connect(
            self,
            &ZStr::from(class),
            ConnectArgs {
                dsn: dsn.as_bytes(),
                username: None,
                password: None,
                options: options.as_ref(),
            },
        );
        self.func = old;
        r
    }

    /// `PDOTest::factory()` over `sqlite::memory:`.
    pub fn factory_class(&mut self, class: &str, options: Option<ZArray>) -> Value {
        let db = self
            .new_pdo(class, "sqlite::memory:", options)
            .expect("connect");
        self.set_attr(&db, php_pdo::consts::ATTR_ERRMODE, i(0))
            .unwrap();
        self.set_attr(&db, php_pdo::consts::ATTR_ERRMODE, i(1))
            .unwrap();
        self.set_attr(&db, php_pdo::consts::ATTR_CASE, i(2))
            .unwrap();
        self.set_attr(
            &db,
            php_pdo::consts::ATTR_STRINGIFY_FETCHES,
            Value::Bool(true),
        )
        .unwrap();
        db
    }

    pub fn factory(&mut self) -> Value {
        self.factory_class("PDO", None)
    }

    pub fn set_attr(&mut self, db: &Value, attr: i64, v: Value) -> RtResult<bool> {
        let old = self.enter("PDO::setAttribute");
        let r = Self::pdo(db).set_attribute(self, attr, &v);
        self.func = old;
        r
    }

    pub fn get_attr(&mut self, db: &Value, attr: i64) -> RtResult<Value> {
        let old = self.enter("PDO::getAttribute");
        let r = Self::pdo(db).get_attribute(self, attr);
        self.func = old;
        r
    }

    pub fn exec(&mut self, db: &Value, sql: &str) -> RtResult<Value> {
        let old = self.enter("PDO::exec");
        let r = Self::pdo(db).exec(self, &z(sql));
        self.func = old;
        r
    }

    pub fn prepare(&mut self, db: &Value, sql: &str) -> RtResult<Value> {
        self.prepare_opts(db, sql, None)
    }

    pub fn prepare_opts(&mut self, db: &Value, sql: &str, opts: Option<ZArray>) -> RtResult<Value> {
        let old = self.enter("PDO::prepare");
        let r = Self::pdo(db).prepare(self, &z(sql), opts.as_ref());
        self.func = old;
        r
    }

    pub fn query(&mut self, db: &Value, sql: &str) -> RtResult<Value> {
        self.query_mode(db, sql, None, &[])
    }

    pub fn query_mode(
        &mut self,
        db: &Value,
        sql: &str,
        mode: Option<i64>,
        args: &[Value],
    ) -> RtResult<Value> {
        let old = self.enter("PDO::query");
        let r = Self::pdo(db).query(self, &z(sql), mode, args);
        self.func = old;
        r
    }

    pub fn quote(&mut self, db: &Value, s: &str, ty: i64) -> RtResult<Value> {
        let old = self.enter("PDO::quote");
        let r = Self::pdo(db).quote(self, &z(s), ty);
        self.func = old;
        r
    }

    pub fn db_error_info(&mut self, db: &Value) -> RtResult<Value> {
        let old = self.enter("PDO::errorInfo");
        let r = Self::pdo(db).error_info(self).map(Value::Array);
        self.func = old;
        r
    }

    pub fn db_error_code(&mut self, db: &Value) -> RtResult<Value> {
        let old = self.enter("PDO::errorCode");
        let r = Self::pdo(db).error_code(self);
        self.func = old;
        r
    }

    pub fn execute(&mut self, st: &Value, params: Option<ZArray>) -> RtResult<bool> {
        let old = self.enter("PDOStatement::execute");
        let r = Self::stmt(st).execute(self, params.as_ref());
        self.func = old;
        r
    }

    pub fn fetch(&mut self, st: &Value, mode: i64) -> RtResult<Value> {
        let old = self.enter("PDOStatement::fetch");
        let r = Self::stmt(st).fetch(self, mode, 0, 0);
        self.func = old;
        r
    }

    pub fn fetch_all(&mut self, st: &Value, mode: Option<i64>, args: &[Value]) -> RtResult<Value> {
        let old = self.enter("PDOStatement::fetchAll");
        let r = Self::stmt(st).fetch_all(self, mode, args).map(Value::Array);
        self.func = old;
        r
    }

    pub fn fetch_column(&mut self, st: &Value, col: i64) -> RtResult<Value> {
        let old = self.enter("PDOStatement::fetchColumn");
        let r = Self::stmt(st).fetch_column(self, col);
        self.func = old;
        r
    }

    pub fn fetch_object(
        &mut self,
        st: &Value,
        class: Option<&str>,
        args: Option<ZArray>,
    ) -> RtResult<Value> {
        let old = self.enter("PDOStatement::fetchObject");
        let c = class.map(ZStr::from);
        let r = Self::stmt(st).fetch_object(self, c.as_ref(), args.as_ref());
        self.func = old;
        r
    }

    pub fn set_fetch_mode(&mut self, st: &Value, mode: i64, args: &[Value]) -> RtResult<bool> {
        let old = self.enter("PDOStatement::setFetchMode");
        let r = Self::stmt(st).set_fetch_mode(self, mode, args);
        self.func = old;
        r
    }

    pub fn bind_param(
        &mut self,
        st: &Value,
        id: ParamId,
        var: &php_runtime::PhpRef,
        ty: i64,
    ) -> RtResult<bool> {
        let old = self.enter("PDOStatement::bindParam");
        let r = Self::stmt(st).bind_param(self, &id, Value::Reference(var.clone()), ty, 0, None);
        self.func = old;
        r
    }

    pub fn bind_column(
        &mut self,
        st: &Value,
        id: ParamId,
        var: &php_runtime::PhpRef,
        ty: i64,
    ) -> RtResult<bool> {
        let old = self.enter("PDOStatement::bindColumn");
        let r = Self::stmt(st).bind_column(self, &id, Value::Reference(var.clone()), ty, 0, None);
        self.func = old;
        r
    }

    pub fn bind_value(&mut self, st: &Value, id: ParamId, v: Value, ty: i64) -> RtResult<bool> {
        let old = self.enter("PDOStatement::bindValue");
        let r = Self::stmt(st).bind_value(self, &id, v, ty);
        self.func = old;
        r
    }

    pub fn close_cursor(&mut self, st: &Value) -> RtResult<bool> {
        let old = self.enter("PDOStatement::closeCursor");
        let r = Self::stmt(st).close_cursor(self);
        self.func = old;
        r
    }

    pub fn stmt_error_info(&mut self, st: &Value) -> RtResult<Value> {
        let old = self.enter("PDOStatement::errorInfo");
        let r = Self::stmt(st).error_info(self).map(Value::Array);
        self.func = old;
        r
    }

    pub fn debug_dump(&mut self, st: &Value) -> RtResult<Value> {
        let old = self.enter("PDOStatement::debugDumpParams");
        let r = Self::stmt(st).debug_dump_params(self);
        self.func = old;
        r
    }

    /// `foreach ($stmt as $k => $v)`.
    pub fn foreach(
        &mut self,
        st: &Value,
        mut f: impl FnMut(&mut TestHost, Value, Value),
    ) -> RtResult<()> {
        let mut it = Self::stmt(st).iterator(self, false)?;
        while it.valid() {
            let k = it.key();
            let v = it.current().unwrap_or(Value::Null);
            f(self, k, v);
            it.move_forward(self)?;
        }
        Ok(())
    }

    /// Runs `f`, printing `Class: message` for an exception.
    pub fn try_print<T>(&mut self, r: RtResult<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                let d = self.describe_error(&e);
                self.echo(&format!("{d}\n"));
                None
            }
        }
    }

    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }
}

// ---------------------------------------------------------------------
// phpt expectations
// ---------------------------------------------------------------------

/// The `--EXPECT--` / `--EXPECTF--` section of a phpt file, with whether
/// it is a format.
pub fn phpt_expect(dir: &str, file: &str) -> Option<(String, bool)> {
    let root = env!("CARGO_MANIFEST_DIR");
    let path = format!("{root}/../../modules/php-src/ext/{dir}/tests/{file}");
    let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
    let mut section = None;
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let t = line.trim_end();
        if t.starts_with("--")
            && t.ends_with("--")
            && t.len() > 4
            && t[2..t.len() - 2]
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_')
        {
            if section.is_some() {
                break;
            }
            match t {
                "--EXPECT--" => section = Some(false),
                "--EXPECTF--" => section = Some(true),
                _ => {}
            }
            continue;
        }
        if section.is_some() {
            out.push_str(line);
        }
    }
    section.map(|f| (out, f))
}

fn normalize_output(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// Turns an expectation into an EXPECTF pattern: literal `%` are escaped
/// for `--EXPECT--`, and object handles (`#4`) become `#%d` because they
/// depend on the VM's allocation.
fn normalize_expect(s: &str, is_format: bool) -> String {
    let s = normalize_output(s);
    let s = if is_format { s } else { s.replace('%', "%%") };
    // `%spdo_034.php`: the script name is the test's
    let mut t = String::new();
    let mut rest = s.as_str();
    while let Some(p) = rest.find("%s") {
        t.push_str(&rest[..p + 2]);
        rest = &rest[p + 2..];
        let ident = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if ident > 0 && rest[ident..].starts_with(".php") {
            rest = &rest[ident..];
        }
    }
    t.push_str(rest);
    let s = t;
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '#' && chars.peek().is_some_and(char::is_ascii_digit) {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
            out.push_str("%d");
        }
    }
    out
}

/// The EXPECTF matcher (`%s`, `%S`, `%d`, `%i`, `%a`, `%A`, `%x`, `%f`,
/// `%c`, `%e`).
pub fn expectf_match(pattern: &str, text: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p[0] == b'%' && p.len() >= 2 {
            let rest = &p[2..];
            let class: Option<(fn(u8) -> bool, bool)> = match p[1] {
                b's' => Some((|c| c != b'\n' && c != b'\r', true)),
                b'S' => Some((|c| c != b'\n' && c != b'\r', false)),
                b'a' => Some((|_| true, true)),
                b'A' => Some((|_| true, false)),
                b'w' => Some((|c: u8| c.is_ascii_whitespace(), false)),
                b'd' => Some((|c: u8| c.is_ascii_digit(), true)),
                b'i' => Some((|c: u8| c.is_ascii_digit() || c == b'-' || c == b'+', true)),
                b'x' => Some((|c: u8| c.is_ascii_hexdigit(), true)),
                b'f' => Some((|c: u8| c.is_ascii_digit() || b".-+eE".contains(&c), true)),
                b'c' => {
                    return !t.is_empty() && m(rest, &t[1..]);
                }
                b'e' => {
                    return !t.is_empty() && t[0] == b'/' && m(rest, &t[1..]);
                }
                b'%' => {
                    return !t.is_empty() && t[0] == b'%' && m(rest, &t[1..]);
                }
                _ => None,
            };
            if let Some((ok, need_one)) = class {
                let mut n = 0;
                while n < t.len() && ok(t[n]) {
                    n += 1;
                }
                let start = usize::from(need_one);
                for k in (start..=n).rev() {
                    if m(rest, &t[k..]) {
                        return true;
                    }
                }
                return false;
            }
        }
        !t.is_empty() && p[0] == t[0] && m(&p[1..], &t[1..])
    }
    m(pattern.as_bytes(), text.as_bytes())
}

/// Compares the output with the phpt expectation; returns `false` (after
/// printing a diff) when they differ.
pub fn check_phpt(dir: &str, file: &str, output: &str) -> bool {
    let Some((expected, is_format)) = phpt_expect(dir, file) else {
        let root = env!("CARGO_MANIFEST_DIR");
        let tests = format!("{root}/../../modules/php-src/ext/{dir}/tests");
        assert!(
            !std::path::Path::new(&tests).exists(),
            "{file}: no EXPECT section found"
        );
        eprintln!("skip {file}: php-src tests not available");
        return true;
    };
    let exp = normalize_expect(&expected, is_format);
    let out = normalize_output(output);
    let ok = expectf_match(&exp, &out);
    if !ok {
        eprintln!("==== {file} EXPECTED ====\n{exp}\n==== GOT ====\n{out}\n====");
    }
    ok
}

/// Asserts the output matches the phpt file's expectation.
#[macro_export]
macro_rules! assert_phpt {
    ($h:expr, $file:expr) => {
        assert!(
            $crate::common::check_phpt("pdo", $file, &$h.output()),
            "output mismatch for {}",
            $file
        );
    };
    ($h:expr, $dir:expr, $file:expr) => {
        assert!(
            $crate::common::check_phpt($dir, $file, &$h.output()),
            "output mismatch for {}",
            $file
        );
    };
}

// ---------------------------------------------------------------------
// An emulating driver: `sqlemu:` = SQLite with PDO-emulated prepares
// ---------------------------------------------------------------------

pub mod emu {
    use php_pdo::driver::{
        ColumnData, ConnectParams, Connection, DbhDriver, DriverEntry, DriverError, ExecuteInfo,
        Prepared, StmtDriver,
    };
    use php_pdo::host::{Ctx, DriverCx};
    use php_runtime::{Value, ZArray, ZStr};
    use std::any::Any;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct EmuDbh {
        inner: Rc<dyn DbhDriver>,
    }

    struct EmuStmt {
        dbh: Rc<dyn DbhDriver>,
        stmt: RefCell<Option<Rc<dyn StmtDriver>>>,
    }

    impl EmuStmt {
        fn inner(&self) -> Result<Rc<dyn StmtDriver>, DriverError> {
            self.stmt.borrow().clone().ok_or(DriverError::Silent)
        }
    }

    fn connect(cx: &mut Ctx<'_>, p: &ConnectParams, o: Option<&ZArray>) -> Option<Connection> {
        let c = (php_pdo::sqlite::SQLITE_DRIVER.factory)(cx, p, o)?;
        Some(Connection {
            driver: Rc::new(EmuDbh { inner: c.driver }),
            flags: c.flags,
        })
    }

    /// Registers the `sqlemu` driver.
    pub fn register() {
        php_pdo::driver::register_driver(DriverEntry {
            name: "sqlemu",
            specific_class: None,
            factory: connect,
        });
    }

    impl DbhDriver for EmuDbh {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn close(&self, cx: &mut DriverCx<'_>) {
            self.inner.close(cx);
        }
        fn prepare(
            &self,
            _: &mut DriverCx<'_>,
            _: &ZStr,
            _: Option<&ZArray>,
        ) -> Result<Prepared, DriverError> {
            Ok(Prepared {
                stmt: Rc::new(EmuStmt {
                    dbh: self.inner.clone(),
                    stmt: RefCell::new(None),
                }),
                supports_placeholders: 0,
                named_rewrite_template: None,
            })
        }
        fn exec(&self, cx: &mut DriverCx<'_>, sql: &ZStr) -> Result<i64, DriverError> {
            self.inner.exec(cx, sql)
        }
        fn quote(
            &self,
            cx: &mut DriverCx<'_>,
            s: &[u8],
            t: i64,
            m: i64,
        ) -> Option<Result<ZStr, DriverError>> {
            self.inner.quote(cx, s, t, m)
        }
        fn set_attribute(
            &self,
            cx: &mut DriverCx<'_>,
            attr: i64,
            v: &Value,
        ) -> Option<Result<(), DriverError>> {
            if attr == php_pdo::consts::ATTR_EMULATE_PREPARES {
                return Some(Ok(()));
            }
            self.inner.set_attribute(cx, attr, v)
        }
        fn get_attribute(
            &self,
            cx: &mut DriverCx<'_>,
            attr: i64,
        ) -> Option<Result<Option<Value>, DriverError>> {
            if attr == php_pdo::consts::ATTR_EMULATE_PREPARES {
                return Some(Ok(Some(Value::Bool(true))));
            }
            self.inner.get_attribute(cx, attr)
        }
        fn fetch_error(&self, s: Option<&dyn StmtDriver>) -> Option<Vec<Value>> {
            self.inner.fetch_error(s)
        }
        fn last_insert_id(
            &self,
            cx: &mut DriverCx<'_>,
            n: Option<&ZStr>,
        ) -> Option<Result<ZStr, DriverError>> {
            self.inner.last_insert_id(cx, n)
        }
        fn scanner(&self) -> &dyn php_sqlite::pdo::parser::Scanner {
            &php_sqlite::pdo::parser::SqliteScanner
        }
    }

    impl StmtDriver for EmuStmt {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn execute(
            &self,
            cx: &mut DriverCx<'_>,
            _: bool,
            active: Option<&ZStr>,
        ) -> Result<ExecuteInfo, DriverError> {
            let sql = active.cloned().unwrap_or_else(ZStr::empty);
            let p = self.dbh.prepare(cx, &sql, None)?;
            *self.stmt.borrow_mut() = Some(p.stmt.clone());
            p.stmt.execute(cx, false, None)
        }
        fn fetch(&self, cx: &mut DriverCx<'_>, o: i64, off: i64) -> Result<bool, DriverError> {
            self.inner()?.fetch(cx, o, off)
        }
        fn describe(&self, cx: &mut DriverCx<'_>, c: i64) -> Result<ColumnData, DriverError> {
            self.inner()?.describe(cx, c)
        }
        fn get_col(&self, cx: &mut DriverCx<'_>, c: i64) -> Result<Value, DriverError> {
            self.inner()?.get_col(cx, c)
        }
        fn cursor_closer(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
            match self.stmt.borrow().clone() {
                Some(s) => s.cursor_closer(cx),
                None => Some(Ok(())),
            }
        }
        fn column_meta(
            &self,
            cx: &mut DriverCx<'_>,
            c: i64,
            e: bool,
        ) -> Option<Result<ZArray, DriverError>> {
            match self.stmt.borrow().clone() {
                Some(s) => s.column_meta(cx, c, e),
                None => Some(Err(DriverError::Silent)),
            }
        }
    }
}

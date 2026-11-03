//! A scripted [`SessionHost`] for lifecycle tests: records diagnostics in
//! PHP's display format, keeps headers, superglobals and `$_SESSION`, and
//! dispatches user save-handler callables to Rust closures.
#![allow(dead_code)]

use php_runtime::{
    CastTarget, CollectingContext, ErrorLevel, Obj, ObjectDump, PhpRef, PlainObject,
    PropertyPurpose, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use php_session::{SessionHost, SessionState};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// A user callable implemented in Rust.
pub type Callback = Rc<dyn Fn(&mut FakeHost, &[Value]) -> RtResult<Option<Value>>>;

/// A fake class: interfaces and (lower-case) method names.
#[derive(Default, Clone)]
pub struct FakeClass {
    pub interfaces: Vec<String>,
    pub methods: Vec<String>,
}

pub struct FakeHost {
    pub ctx: CollectingContext,
    pub state: SessionState,
    /// Rendered diagnostics: `"Warning: session_start(): msg"`.
    pub log: Vec<String>,
    /// The function `php_error_docref` attributes messages to.
    pub func: String,
    pub headers: Vec<String>,
    pub headers_sent: bool,
    pub output_start: Option<(String, u32)>,
    pub globals: HashMap<String, ZArray>,
    pub server: HashMap<String, Value>,
    pub session_global: Option<PhpRef>,
    pub callbacks: HashMap<String, Callback>,
    pub classes: HashMap<String, FakeClass>,
    pub now: i64,
    pub location: Option<(String, u32)>,
    pub rng_counter: u8,
    pub sid_constant: Option<Vec<u8>>,
    pub script: Option<PathBuf>,
    pub temp: PathBuf,
    pub shutdown_registered: bool,
    pub error_handler_throws: bool,
    pub deny_basedir: bool,
}

impl FakeHost {
    pub fn new() -> Self {
        Self {
            ctx: CollectingContext::new(),
            state: SessionState::new(),
            log: Vec::new(),
            func: "Unknown".into(),
            headers: Vec::new(),
            headers_sent: false,
            output_start: None,
            globals: HashMap::new(),
            server: HashMap::new(),
            session_global: None,
            callbacks: HashMap::new(),
            classes: HashMap::new(),
            now: 1_700_000_000,
            location: Some(("/t/test.php".into(), 3)),
            rng_counter: 0,
            sid_constant: None,
            script: None,
            temp: std::env::temp_dir(),
            shutdown_registered: false,
            error_handler_throws: false,
            deny_basedir: false,
        }
    }

    /// A host after `MINIT` with the given `-d` settings and `RINIT`.
    pub fn with_ini(ini: &[(&str, &str)]) -> Self {
        let mut h = Self::new();
        let map: HashMap<String, Vec<u8>> = ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.as_bytes().to_vec()))
            .collect();
        h.func = "PHP Startup".into();
        php_session::module_startup(&mut h, &|name| map.get(name).cloned());
        h.func = "PHP Request Startup".into();
        php_session::request_startup(&mut h).unwrap();
        h.func = "Unknown".into();
        h
    }

    /// Runs `f` as the PHP function `func`.
    pub fn call<T>(&mut self, func: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.func, func.to_string());
        let r = f(self);
        self.func = saved;
        r
    }

    /// The current `$_SESSION` value.
    pub fn session(&self) -> Value {
        self.session_global
            .as_ref()
            .map_or(Value::Null, PhpRef::get)
    }

    /// `$_SESSION[key] = value`.
    pub fn set_var(&mut self, key: &str, v: Value) {
        let r = self
            .session_global
            .get_or_insert_with(|| PhpRef::new(Value::empty_array()))
            .clone();
        let mut b = r.borrow_mut();
        if let Some(t) = b.as_array_mut() {
            t.insert(key, v);
        }
    }

    pub fn session_len(&self) -> usize {
        self.session().as_array().map_or(0, |a| a.len())
    }

    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    pub fn set_global(&mut self, name: &str, pairs: &[(&str, Value)]) {
        let mut a = ZArray::new();
        for (k, v) in pairs {
            a.make_mut().insert(*k, v.clone());
        }
        self.globals.insert(name.into(), a);
    }

    pub fn on(
        &mut self,
        name: &str,
        f: impl Fn(&mut FakeHost, &[Value]) -> RtResult<Option<Value>> + 'static,
    ) {
        self.callbacks.insert(name.to_string(), Rc::new(f));
    }

    /// Registers procedural callbacks `open`… returning the given values.
    pub fn simple_handlers(&mut self, read_data: &'static str) {
        self.on("open", |_, _| Ok(Some(Value::Bool(true))));
        self.on("close", |_, _| Ok(Some(Value::Bool(true))));
        self.on("read", move |_, _| Ok(Some(Value::string(read_data))));
        self.on("write", |_, _| Ok(Some(Value::Bool(true))));
        self.on("destroy", |_, _| Ok(Some(Value::Bool(true))));
        self.on("gc", |_, _| Ok(Some(Value::Int(0))));
    }

    pub fn procedural_args(names: &[&str]) -> Vec<Value> {
        names.iter().map(|n| Value::string(*n)).collect()
    }

    /// An object of a fake class.
    pub fn object(&mut self, class: &str, interfaces: &[&str], methods: &[&str]) -> Value {
        self.classes.insert(
            class.to_string(),
            FakeClass {
                interfaces: interfaces.iter().map(|s| (*s).to_string()).collect(),
                methods: methods.iter().map(|s| s.to_ascii_lowercase()).collect(),
            },
        );
        Value::Object(PlainObject::create(class, ZArray::new()))
    }

    fn callback_key(callable: &Value) -> Option<String> {
        match callable.deref() {
            Value::Str(s) => Some(s.to_string_lossy()),
            Value::Array(a) => {
                let obj = a.get_int(0)?.deref();
                let m = a.get_int(1)?.deref();
                let class = match obj {
                    Value::Object(o) => o.class_name().to_string_lossy(),
                    Value::Str(s) => s.to_string_lossy(),
                    _ => return None,
                };
                Some(format!(
                    "{class}::{}",
                    m.as_str()?.to_string_lossy().to_ascii_lowercase()
                ))
            }
            _ => None,
        }
    }
}

impl RuntimeContext for FakeHost {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.log.push(format!("{}: {message}", level.label()));
        if self.error_handler_throws {
            return Err(RuntimeError::Error(format!("handler: {message}")));
        }
        Ok(())
    }
    fn object_properties(&mut self, obj: &Obj, p: PropertyPurpose) -> RtResult<Option<ZArray>> {
        self.ctx.object_properties(obj, p)
    }
    fn object_dump(&mut self, obj: &Obj, p: PropertyPurpose) -> RtResult<ObjectDump> {
        self.ctx.object_dump(obj, p)
    }
    fn cast_object(&mut self, obj: &Obj, t: CastTarget) -> RtResult<Option<Value>> {
        self.ctx.cast_object(obj, t)
    }
    fn serialize_object(
        &mut self,
        obj: &Obj,
    ) -> RtResult<php_runtime::serialize::SerializedObject> {
        self.ctx.serialize_object(obj)
    }
    fn unserialize_object_create(&mut self, class: &ZStr) -> RtResult<Value> {
        self.ctx.unserialize_object_create(class)
    }
    fn unserialize_object_finish(&mut self, obj: &Value, props: ZArray) -> RtResult<()> {
        self.ctx.unserialize_object_finish(obj, props)
    }
    fn unserialize_custom(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        self.ctx.unserialize_custom(class, data)
    }
    fn property_write(&mut self, obj: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
        self.ctx.property_write(obj, name, value)
    }
    fn create_std_object(&mut self, props: ZArray) -> RtResult<Value> {
        self.ctx.create_std_object(props)
    }
}

impl SessionHost for FakeHost {
    fn session(&mut self) -> &mut SessionState {
        &mut self.state
    }

    fn emit_diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        let msg = format!("{}: {}: {message}", level.label(), self.prefix());
        self.log.push(msg);
        if self.error_handler_throws {
            return Err(RuntimeError::Error(format!("handler: {message}")));
        }
        Ok(())
    }

    fn call_handler(&mut self, callable: &Value, args: &[Value]) -> RtResult<Option<Value>> {
        let key = Self::callback_key(callable).unwrap_or_default();
        let Some(cb) = self.callbacks.get(&key).cloned() else {
            return Err(RuntimeError::Error(format!(
                "Call to undefined function {key}()"
            )));
        };
        let display = key
            .split_once("::")
            .map_or(key.clone(), |(c, m)| format!("{c}::{m}"));
        self.call(&display, |h| cb(h, args))
    }

    fn check_callable(&mut self, callable: &Value) -> Result<(), String> {
        match Self::callback_key(callable) {
            Some(k) if self.callbacks.contains_key(&k) => Ok(()),
            Some(k) if !k.contains("::") => Err(format!(
                "function \"{k}\" not found or invalid function name"
            )),
            _ => Err("first array member is not a valid class name or object".into()),
        }
    }

    fn instance_of(&mut self, obj: &Obj, class: &str) -> bool {
        let name = obj.class_name().to_string_lossy();
        name == class
            || self
                .classes
                .get(&name)
                .is_some_and(|c| c.interfaces.iter().any(|i| i == class))
    }

    fn has_method(&mut self, obj: &Obj, method_lc: &str) -> bool {
        let name = obj.class_name().to_string_lossy();
        self.classes
            .get(&name)
            .is_some_and(|c| c.methods.iter().any(|m| m == method_lc))
    }

    fn bind_session_global(&mut self, slot: Option<PhpRef>) {
        self.session_global = slot;
    }

    fn headers_sent(&self) -> bool {
        self.headers_sent
    }

    fn output_start(&self) -> Option<(String, u32)> {
        self.output_start.clone()
    }

    fn add_header(&mut self, line: &[u8], replace: bool) {
        let line = String::from_utf8_lossy(line).into_owned();
        if replace {
            let name = line
                .split(':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            self.headers
                .retain(|h| h.split(':').next().unwrap_or_default().to_ascii_lowercase() != name);
        }
        self.headers.push(line);
    }

    fn delete_header_prefix(&mut self, prefix: &[u8]) {
        let p = String::from_utf8_lossy(prefix).to_ascii_lowercase();
        self.headers
            .retain(|h| !(h.len() > p.len() && h.to_ascii_lowercase().starts_with(&p)));
    }

    fn request_global(&mut self, name: &str) -> Option<ZArray> {
        self.globals.get(name).cloned()
    }

    fn server_var(&mut self, name: &str) -> Option<Value> {
        self.server.get(name).cloned()
    }

    fn script_path(&self) -> Option<PathBuf> {
        self.script.clone()
    }

    fn now(&self) -> i64 {
        self.now
    }

    fn executing_location(&self) -> Option<(String, u32)> {
        self.location.clone()
    }

    fn temp_dir(&self) -> PathBuf {
        self.temp.clone()
    }

    fn random_bytes(&mut self, buf: &mut [u8]) -> RtResult<()> {
        // Deterministic: each ID differs by its first byte.
        self.rng_counter = self.rng_counter.wrapping_add(1);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = if i == 0 {
                self.rng_counter
            } else {
                u8::try_from(i % 256).unwrap_or(0)
            };
        }
        Ok(())
    }

    fn gc_random(&mut self, max: i64) -> i64 {
        max
    }

    fn open_basedir_denied(&mut self, path: &[u8]) -> bool {
        if self.deny_basedir {
            let msg = format!(
                "open_basedir restriction in effect. File({}) is not within the allowed path(s): (.)",
                String::from_utf8_lossy(path)
            );
            let _ = self.emit_diagnostic(php_runtime::ErrorLevel::Warning, &msg);
        }
        self.deny_basedir
    }

    fn define_sid(&mut self, value: &[u8]) {
        self.sid_constant = Some(value.to_vec());
    }

    fn register_session_shutdown(&mut self) -> bool {
        self.shutdown_registered = true;
        true
    }

    fn remove_session_shutdown(&mut self) {
        self.shutdown_registered = false;
    }
}

impl FakeHost {
    fn prefix(&self) -> String {
        if self.func.starts_with("PHP ") || self.func == "Unknown" {
            self.func.clone()
        } else {
            format!("{}()", self.func)
        }
    }
}

/// A fresh temporary directory.
pub fn tempdir(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "php-session-test-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

pub fn s(v: &str) -> ZStr {
    ZStr::from(v)
}

/// A compact `var_dump`-like rendering of scalars.
pub fn show(v: &Value) -> String {
    match v.deref() {
        Value::Null => "NULL".into(),
        Value::Bool(b) => format!("bool({b})"),
        Value::Int(i) => format!("int({i})"),
        Value::Float(f) => format!("float({f})"),
        Value::Str(s) => format!("string({}) \"{}\"", s.len(), s.to_string_lossy()),
        Value::Array(a) => format!("array({})", a.len()),
        other => other.type_name(),
    }
}

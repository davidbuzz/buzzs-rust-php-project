//! A minimal [`Host`] for the integration tests: user callables are Rust
//! closures registered by name, diagnostics are logged as PHP prints them.

#![allow(dead_code)]

use php_runtime::{
    CastTarget, CollectingContext, ErrorLevel, Obj, PlainObject, RtResult, RuntimeContext,
    RuntimeError, Value, ZArray, ZStr,
};
use php_sqlite::Host;
use std::collections::HashMap;
use std::rc::Rc;

/// A user callable.
pub type Cb = Rc<dyn Fn(&mut TestHost, &[Value]) -> Result<Value, RuntimeError>>;

/// The test host.
#[derive(Default)]
pub struct TestHost {
    pub ctx: CollectingContext,
    /// Printed diagnostics (`"Warning: SQLite3::exec(): msg"`).
    pub log: Vec<String>,
    pub funcs: HashMap<String, Cb>,
    /// The active function used as `php_error_docref` prefix.
    pub current: String,
    pub extension_dir: Option<Vec<u8>>,
    pub open_basedir: Option<Vec<u8>>,
    /// Stream resources by handle: their remaining contents.
    pub streams: HashMap<i64, Vec<u8>>,
}

impl TestHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a callable and returns its value (a function name string).
    pub fn func(
        &mut self,
        name: &str,
        f: impl Fn(&mut TestHost, &[Value]) -> Result<Value, RuntimeError> + 'static,
    ) -> Value {
        self.funcs.insert(name.to_string(), Rc::new(f));
        Value::from(name)
    }

    /// Sets the docref prefix.
    pub fn at(&mut self, func: &str) -> &mut Self {
        self.current = func.to_string();
        self
    }

    /// Takes the log.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }
}

impl RuntimeContext for TestHost {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.log.push(format!("{}: {}", level.label(), message));
        Ok(())
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        if obj.class_name().as_bytes() == b"throws" {
            return Err(RuntimeError::Custom {
                class: ZStr::from("Exception"),
                message: "Sorry".into(),
            });
        }
        self.ctx.cast_object(obj, target)
    }
}

impl Host for TestHost {
    fn call(&mut self, callable: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
        let name = callable
            .as_str()
            .map(ZStr::to_string_lossy)
            .unwrap_or_default();
        let Some(f) = self.funcs.get(&name).cloned() else {
            return Err(RuntimeError::Error(format!(
                "Call to undefined function {name}()"
            )));
        };
        f(self, args)
    }

    fn docref(&mut self, level: ErrorLevel, message: &str) -> Result<(), RuntimeError> {
        let prefix = if self.current.is_empty() {
            String::new()
        } else {
            format!("{}(): ", self.current)
        };
        self.log
            .push(format!("{}: {prefix}{message}", level.label()));
        Ok(())
    }

    fn active_function_name(&self) -> String {
        self.current.clone()
    }

    fn read_stream(&mut self, value: &Value) -> Option<Vec<u8>> {
        match value.deref() {
            Value::Resource(r) if r.type_name().is_some_and(|t| t.as_bytes() == b"stream") => {
                Some(self.streams.remove(&r.handle()).unwrap_or_default())
            }
            _ => None,
        }
    }

    fn open_basedir_active(&self) -> bool {
        self.open_basedir.is_some()
    }

    fn check_open_basedir(&mut self, path: &[u8]) -> bool {
        match &self.open_basedir {
            Some(dir) if !path.starts_with(dir) => {
                self.log.push(format!(
                    "Warning: open_basedir restriction in effect. File({}) is not within the allowed path(s): ({})",
                    String::from_utf8_lossy(path),
                    String::from_utf8_lossy(dir)
                ));
                true
            }
            _ => false,
        }
    }

    fn sqlite3_extension_dir(&self) -> Option<Vec<u8>> {
        self.extension_dir.clone()
    }
}

/// `var_dump()` output of a value.
pub fn dump(v: &Value) -> String {
    let mut out = Vec::new();
    let mut ctx = CollectingContext::new();
    php_runtime::dump::var_dump(&mut out, v, &mut ctx).expect("var_dump");
    String::from_utf8_lossy(&out).into_owned()
}

/// `print_r()` output of a value.
pub fn print_r(v: &Value) -> String {
    let mut ctx = CollectingContext::new();
    String::from_utf8_lossy(&php_runtime::dump::print_r(v, &mut ctx).expect("print_r")).into_owned()
}

/// A string value.
pub fn s(v: &str) -> Value {
    Value::from(v)
}

/// An object whose `__toString()` throws `Exception("Sorry")`.
pub fn throws_object() -> Value {
    Value::Object(PlainObject::create("throws", ZArray::new()))
}

/// Strips the `%d` parts the phpt files leave open: replaces every integer
/// after `int(` by `%d`.
pub fn mask_ints(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(pos) = rest.find("int(") {
        out.push_str(&rest[..pos + 4]);
        rest = &rest[pos + 4..];
        let end = rest.find(')').unwrap_or(0);
        out.push_str("%d");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

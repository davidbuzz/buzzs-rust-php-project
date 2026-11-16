// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A [`FilterHost`] for tests: collects diagnostics, knows a few regexes and
//! callbacks by name, and marks `Filter\FilterFailedException`s.

use crate::host::FilterHost;
use php_runtime::{
    CastTarget, Diagnostic, ErrorLevel, Obj, PhpObject, RtResult, RuntimeContext, RuntimeError,
    Value, ZStr,
};
use std::any::Any;

/// A test object: `to_string` is the `__toString()` result, if any.
pub struct TestObject {
    pub class: ZStr,
    pub to_string: Option<ZStr>,
}

impl PhpObject for TestObject {
    fn handle(&self) -> u32 {
        1
    }
    fn class_name(&self) -> ZStr {
        self.class.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Builds a test object value.
pub fn object(class: &str, to_string: Option<&[u8]>) -> Value {
    Value::Object(Obj::new(TestObject {
        class: ZStr::from(class),
        to_string: to_string.map(ZStr::from),
    }))
}

/// A regex stand-in: a pattern string and a matcher.
pub type Regex = (&'static str, fn(&[u8]) -> bool);

#[derive(Default)]
pub struct TestHost {
    pub diagnostics: Vec<Diagnostic>,
    pub regexes: Vec<Regex>,
}

/// The prefix `filter_failed_exception` puts on its messages.
pub const FAILED_PREFIX: &str = "FilterFailedException: ";

impl TestHost {
    pub fn messages(&self) -> Vec<String> {
        self.diagnostics
            .iter()
            .map(|d| format!("{}: {}", d.level.label(), d.message))
            .collect()
    }
}

impl RuntimeContext for TestHost {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.diagnostics.push(Diagnostic {
            level,
            message: message.to_string(),
        });
        Ok(())
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        let t = obj.downcast_ref::<TestObject>();
        Ok(match target {
            CastTarget::String => t.and_then(|t| t.to_string.clone()).map(Value::Str),
            CastTarget::Bool => Some(Value::Bool(true)),
            _ => None,
        })
    }
}

impl FilterHost for TestHost {
    fn regex_match(&mut self, pattern: &ZStr, subject: &[u8]) -> RtResult<Option<bool>> {
        Ok(self
            .regexes
            .iter()
            .find(|(p, _)| p.as_bytes() == pattern.as_bytes())
            .map(|(_, f)| f(subject)))
    }

    fn is_callable(&mut self, callable: &Value) -> bool {
        matches!(callable.deref(), Value::Str(s) if matches!(s.as_bytes(), b"strtoupper" | b"identity" | b"to_null" | b"thrower"))
    }

    fn call_callback(&mut self, callable: &Value, arg: Value) -> RtResult<Value> {
        let Value::Str(name) = callable.deref() else {
            return Ok(Value::Null);
        };
        Ok(match name.as_bytes() {
            b"strtoupper" => match arg {
                Value::Str(s) => Value::Str(s.to_ascii_uppercase()),
                other => other,
            },
            b"identity" => arg,
            b"thrower" => return Err(RuntimeError::Error("thrown from callback".into())),
            _ => Value::Null,
        })
    }

    fn filter_failed_exception(&mut self, message: String) -> RuntimeError {
        RuntimeError::Error(format!("{FAILED_PREFIX}{message}"))
    }
}

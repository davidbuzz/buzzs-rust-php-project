// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `user` save handler (`ext/session/mod_user.c`): bridges the module
//! operations to the callables registered with `session_set_save_handler()`
//! through [`SessionHost::call_handler`].

use crate::host::SessionHost;
use crate::state::{create_default_id, throw, warn};
use php_runtime::{ErrorLevel, RuntimeError, Value, ZStr};

/// The registered callables (`PS(mod_user_names)`); `None` is `UNDEF`.
#[derive(Debug, Clone, Default)]
pub struct UserHandlers {
    /// `open(string $path, string $name): bool`.
    pub open: Option<Value>,
    /// `close(): bool`.
    pub close: Option<Value>,
    /// `read(string $id): string|false`.
    pub read: Option<Value>,
    /// `write(string $id, string $data): bool`.
    pub write: Option<Value>,
    /// `destroy(string $id): bool`.
    pub destroy: Option<Value>,
    /// `gc(int $max_lifetime): int|false`.
    pub gc: Option<Value>,
    /// `create_sid(): string` (optional).
    pub create_sid: Option<Value>,
    /// `validateId(string $id): bool` (optional).
    pub validate_sid: Option<Value>,
    /// `updateTimestamp(string $id, string $data): bool` (optional).
    pub update_timestamp: Option<Value>,
}

/// Which callable to invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Open,
    Close,
    Read,
    Write,
    Destroy,
    Gc,
    CreateSid,
    ValidateSid,
    UpdateTimestamp,
}

impl UserHandlers {
    pub(crate) fn get(&self, slot: Slot) -> Option<&Value> {
        match slot {
            Slot::Open => self.open.as_ref(),
            Slot::Close => self.close.as_ref(),
            Slot::Read => self.read.as_ref(),
            Slot::Write => self.write.as_ref(),
            Slot::Destroy => self.destroy.as_ref(),
            Slot::Gc => self.gc.as_ref(),
            Slot::CreateSid => self.create_sid.as_ref(),
            Slot::ValidateSid => self.validate_sid.as_ref(),
            Slot::UpdateTimestamp => self.update_timestamp.as_ref(),
        }
    }
}

/// `ps_call_handler()`: `None` is an `UNDEF` return value (recursion,
/// pending exception or failed call).
pub(crate) fn call(h: &mut dyn SessionHost, slot: Slot, args: &[Value]) -> Option<Value> {
    if h.session().in_save_handler {
        h.session().in_save_handler = false;
        warn(
            h,
            ErrorLevel::Warning,
            "Cannot call session save handler in a recursive manner",
        );
        return None;
    }
    let func = h.session().user.get(slot).cloned()?;
    // zend_call_function() does nothing while an exception is pending.
    if h.session().has_exception() {
        return None;
    }
    h.session().in_save_handler = true;
    let r = h.call_handler(&func, args);
    match r {
        Ok(v) => {
            h.session().in_save_handler = false;
            v
        }
        Err(e) => {
            // A fatal error (bailout) longjmps past the reset, so the next
            // handler call reports the recursion, as PHP does at shutdown.
            if !matches!(e, RuntimeError::Fatal(_)) {
                h.session().in_save_handler = false;
            }
            h.session().set_exception(e);
            None
        }
    }
}

/// `verify_bool_return_type_userland_calls()`.
pub(crate) fn verify_bool(h: &mut dyn SessionHost, v: Option<&Value>) -> bool {
    let v = v.map(Value::deref);
    match v {
        Some(Value::Bool(true)) => true,
        None | Some(Value::Bool(false)) => false,
        Some(Value::Int(n)) if n == 0 || n == -1 => {
            warn(
                h,
                ErrorLevel::Deprecated,
                "Session callback must have a return value of type bool, int returned",
            );
            n == 0
        }
        Some(other) => {
            throw(
                h,
                RuntimeError::TypeError(format!(
                    "Session callback must have a return value of type bool, {} returned",
                    other.value_name()
                )),
            );
            false
        }
    }
}

fn s(b: &[u8]) -> Value {
    Value::Str(ZStr::from(b))
}

/// `PS_OPEN_FUNC(user)`.
pub(crate) fn open(h: &mut dyn SessionHost, save_path: &[u8], name: &[u8]) -> bool {
    let r = call(h, Slot::Open, &[s(save_path), s(name)]);
    h.session().mod_user_implemented = true;
    verify_bool(h, r.as_ref())
}

/// `PS_CLOSE_FUNC(user)`.
pub(crate) fn close(h: &mut dyn SessionHost) -> bool {
    if !h.session().mod_user_implemented {
        return true;
    }
    let r = call(h, Slot::Close, &[]);
    h.session().mod_user_implemented = false;
    verify_bool(h, r.as_ref())
}

/// `PS_READ_FUNC(user)`.
pub(crate) fn read(h: &mut dyn SessionHost, key: &[u8]) -> Option<Vec<u8>> {
    match call(h, Slot::Read, &[s(key)])?.deref() {
        Value::Str(v) => Some(v.as_bytes().to_vec()),
        _ => None,
    }
}

/// `PS_WRITE_FUNC(user)`.
pub(crate) fn write(h: &mut dyn SessionHost, key: &[u8], val: &[u8]) -> bool {
    let r = call(h, Slot::Write, &[s(key), s(val)]);
    verify_bool(h, r.as_ref())
}

/// `PS_DESTROY_FUNC(user)`.
pub(crate) fn destroy(h: &mut dyn SessionHost, key: &[u8]) -> bool {
    let r = call(h, Slot::Destroy, &[s(key)]);
    verify_bool(h, r.as_ref())
}

/// `PS_GC_FUNC(user)`.
pub(crate) fn gc(h: &mut dyn SessionHost, maxlifetime: i64) -> i64 {
    match call(h, Slot::Gc, &[Value::Int(maxlifetime)]).map(|v| v.deref()) {
        Some(Value::Int(n)) => n,
        Some(Value::Bool(true)) => 1,
        _ => -1,
    }
}

/// `PS_CREATE_SID_FUNC(user)`.
pub(crate) fn create_sid(h: &mut dyn SessionHost) -> Option<Vec<u8>> {
    if h.session().user.create_sid.is_none() {
        return create_default_id(h);
    }
    match call(h, Slot::CreateSid, &[])?.deref() {
        Value::Str(v) => Some(v.as_bytes().to_vec()),
        other => {
            throw(
                h,
                RuntimeError::TypeError(format!(
                    "Session id must be of type string, {} given",
                    other.type_name()
                )),
            );
            None
        }
    }
}

/// `PS_VALIDATE_SID_FUNC(user)`.
pub(crate) fn validate_sid(h: &mut dyn SessionHost, key: &[u8]) -> bool {
    if h.session().user.validate_sid.is_none() {
        return true;
    }
    let r = call(h, Slot::ValidateSid, &[s(key)]);
    verify_bool(h, r.as_ref())
}

/// `PS_UPDATE_TIMESTAMP_FUNC(user)` (falls back to `write`).
pub(crate) fn update_timestamp(h: &mut dyn SessionHost, key: &[u8], val: &[u8]) -> bool {
    let slot = if h.session().user.update_timestamp.is_some() {
        Slot::UpdateTimestamp
    } else {
        Slot::Write
    };
    let r = call(h, slot, &[s(key), s(val)]);
    verify_bool(h, r.as_ref())
}

/// `zend_get_callable_name()` for the "Failed to write session data using
/// user defined save handler" warning.
#[must_use]
pub fn callable_name(v: &Value) -> String {
    match v.deref() {
        Value::Str(s) => s.to_string_lossy(),
        Value::Array(a) if a.len() == 2 => {
            let first = a.get_int(0).map(Value::deref);
            let method = a
                .get_int(1)
                .map(|m| match m.deref() {
                    Value::Str(s) => s.to_string_lossy(),
                    other => other.type_name(),
                })
                .unwrap_or_default();
            match first {
                Some(Value::Object(o)) => format!("{}::{method}", o.class_name().to_string_lossy()),
                Some(Value::Str(c)) => format!("{}::{method}", c.to_string_lossy()),
                _ => "Array".to_string(),
            }
        }
        Value::Array(_) => "Array".to_string(),
        Value::Object(o) => format!("{}::__invoke", o.class_name().to_string_lossy()),
        other => other.type_name(),
    }
}

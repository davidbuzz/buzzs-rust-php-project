// Derived from php-src ext/session/mod_user_class.c. Similarity metric to original: 42%
//   Copyright © The PHP Group and Contributors. Author: Arpad Ray <arpad@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The session classes and interfaces (`session.stub.php`,
//! `mod_user_class.c`): declarations for the VM to register, the
//! `SessionHandler` methods delegating to the previous (default) module,
//! and the `interface_gets_implemented` hook of `SessionHandlerInterface`.

use crate::handler::{
    mod_close, mod_create_sid, mod_destroy, mod_gc, mod_open, mod_read, mod_validate_sid,
    mod_write, ModRef,
};
use crate::host::SessionHost;
use crate::state::{create_default_id, finish, throw, warn, SessionStatus};
use php_runtime::{ErrorLevel, RtResult, RuntimeError, Value, ZStr};

/// A method parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDecl {
    /// Name without `$`.
    pub name: &'static str,
    /// Declared type.
    pub ty: &'static str,
}

/// A method declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDecl {
    /// Method name (declared case).
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [ParamDecl],
    /// Return type (all are `@tentative-return-type`).
    pub return_type: &'static str,
}

/// A class or interface declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassDecl {
    /// Class name.
    pub name: &'static str,
    /// Whether it is an interface (methods are abstract).
    pub is_interface: bool,
    /// Implemented interfaces.
    pub interfaces: &'static [&'static str],
    /// Methods (implemented natively for `SessionHandler`).
    pub methods: &'static [MethodDecl],
}

const fn p(name: &'static str, ty: &'static str) -> ParamDecl {
    ParamDecl { name, ty }
}

const OPEN: MethodDecl = MethodDecl {
    name: "open",
    params: &[p("path", "string"), p("name", "string")],
    return_type: "bool",
};
const CLOSE: MethodDecl = MethodDecl {
    name: "close",
    params: &[],
    return_type: "bool",
};
const READ: MethodDecl = MethodDecl {
    name: "read",
    params: &[p("id", "string")],
    return_type: "string|false",
};
const WRITE: MethodDecl = MethodDecl {
    name: "write",
    params: &[p("id", "string"), p("data", "string")],
    return_type: "bool",
};
const DESTROY: MethodDecl = MethodDecl {
    name: "destroy",
    params: &[p("id", "string")],
    return_type: "bool",
};
const GC: MethodDecl = MethodDecl {
    name: "gc",
    params: &[p("max_lifetime", "int")],
    return_type: "int|false",
};
const CREATE_SID: MethodDecl = MethodDecl {
    name: "create_sid",
    params: &[],
    return_type: "string",
};
const VALIDATE_ID: MethodDecl = MethodDecl {
    name: "validateId",
    params: &[p("id", "string")],
    return_type: "bool",
};
const UPDATE_TIMESTAMP: MethodDecl = MethodDecl {
    name: "updateTimestamp",
    params: &[p("id", "string"), p("data", "string")],
    return_type: "bool",
};

/// The classes in registration order.
pub const CLASSES: &[ClassDecl] = &[
    ClassDecl {
        name: "SessionHandlerInterface",
        is_interface: true,
        interfaces: &[],
        methods: &[OPEN, CLOSE, READ, WRITE, DESTROY, GC],
    },
    ClassDecl {
        name: "SessionIdInterface",
        is_interface: true,
        interfaces: &[],
        methods: &[CREATE_SID],
    },
    ClassDecl {
        name: "SessionUpdateTimestampHandlerInterface",
        is_interface: true,
        interfaces: &[],
        methods: &[VALIDATE_ID, UPDATE_TIMESTAMP],
    },
    ClassDecl {
        name: "SessionHandler",
        is_interface: false,
        interfaces: &["SessionHandlerInterface", "SessionIdInterface"],
        methods: &[
            OPEN,
            CLOSE,
            READ,
            WRITE,
            DESTROY,
            GC,
            CREATE_SID,
            VALIDATE_ID,
        ],
    },
];

/// The class table for the VM to register.
#[must_use]
pub fn class_table() -> &'static [ClassDecl] {
    CLASSES
}

/// `PHP_SESSION_*` constants.
pub const CONSTANTS: [(&str, i64); 3] = [
    ("PHP_SESSION_DISABLED", 0),
    ("PHP_SESSION_NONE", 1),
    ("PHP_SESSION_ACTIVE", 2),
];

/// Every function of the extension, in `session.stub.php` order.
pub const FUNCTIONS: &[&str] = &[
    "session_name",
    "session_module_name",
    "session_save_path",
    "session_id",
    "session_create_id",
    "session_regenerate_id",
    "session_decode",
    "session_encode",
    "session_destroy",
    "session_unset",
    "session_gc",
    "session_get_cookie_params",
    "session_write_close",
    "session_abort",
    "session_reset",
    "session_status",
    "session_register_shutdown",
    "session_commit",
    "session_set_save_handler",
    "session_cache_limiter",
    "session_cache_expire",
    "session_set_cookie_params",
    "session_start",
];

/// `session_handler_interface_gets_implemented()`: the `E_WARNING`s
/// (`zend_error`, no function prefix) for a class implementing
/// `SessionHandlerInterface` without the PHP 9 methods.
/// `has_*` are method-table lookups, `implements_*` whether any directly
/// implemented interface is (or extends) `SessionIdInterface` /
/// `SessionUpdateTimestampHandlerInterface`.
#[must_use]
#[allow(clippy::fn_params_excessive_bools)] // four independent class facts
pub fn handler_interface_implemented(
    class: &str,
    has_create_sid: bool,
    implements_id_interface: bool,
    has_validate_id: bool,
    implements_update_interface: bool,
) -> Vec<String> {
    let mut v = Vec::new();
    if !has_create_sid && !implements_id_interface {
        v.push(format!(
            "Class {class} implementing SessionHandlerInterface is missing the create_sid() \
             method which will be required in PHP 9.0"
        ));
    }
    if !has_validate_id && !implements_update_interface {
        v.push(format!(
            "Class {class} implementing SessionHandlerInterface is missing the validateId() \
             method which will be required in PHP 9.0"
        ));
    }
    v
}

/// `PS_SANITY_CHECK`: the default module, or the thrown `Error`.
fn sanity(h: &mut dyn SessionHost) -> RtResult<ModRef> {
    if h.session().status != SessionStatus::Active {
        return Err(RuntimeError::Error("Session is not active".into()));
    }
    h.session()
        .default_mod
        .clone()
        .ok_or_else(|| RuntimeError::Error("Cannot call default session handler".into()))
}

/// `PS_SANITY_CHECK_IS_OPEN`: `Ok(None)` after the "not open" warning.
fn sanity_open(h: &mut dyn SessionHost) -> RtResult<Option<ModRef>> {
    let m = sanity(h)?;
    if !h.session().mod_user_is_open {
        warn(h, ErrorLevel::Warning, "Parent session handler is not open");
        return finish(h, None);
    }
    Ok(Some(m))
}

/// `SessionHandler::open(string $path, string $name): bool`.
pub fn session_handler_open(h: &mut dyn SessionHost, path: &[u8], name: &[u8]) -> RtResult<bool> {
    let m = sanity(h)?;
    let ok = mod_open(h, &m, path, name);
    if ok {
        h.session().mod_user_is_open = true;
    }
    finish(h, ok)
}

/// `SessionHandler::close(): bool`.
///
/// PHP closes the parent even when argument parsing fails
/// (`zend_parse_parameters_none()` is not checked), so the VM should call
/// this after raising the `ArgumentCountError` for extra arguments.
pub fn session_handler_close(h: &mut dyn SessionHost) -> RtResult<bool> {
    let Some(m) = sanity_open(h)? else {
        return Ok(false);
    };
    h.session().mod_user_is_open = false;
    let ok = mod_close(h, &m);
    finish(h, ok)
}

/// `SessionHandler::read(string $id): string|false`.
pub fn session_handler_read(h: &mut dyn SessionHost, id: &[u8]) -> RtResult<Value> {
    let Some(m) = sanity_open(h)? else {
        return Ok(Value::Bool(false));
    };
    let maxlifetime = h.session().config.gc_maxlifetime;
    let v = match mod_read(h, &m, id, maxlifetime) {
        Some(data) => Value::Str(ZStr::from(data)),
        None => Value::Bool(false),
    };
    finish(h, v)
}

/// `SessionHandler::write(string $id, string $data): bool`.
pub fn session_handler_write(h: &mut dyn SessionHost, id: &[u8], data: &[u8]) -> RtResult<bool> {
    let Some(m) = sanity_open(h)? else {
        return Ok(false);
    };
    let maxlifetime = h.session().config.gc_maxlifetime;
    let ok = mod_write(h, &m, id, data, maxlifetime);
    finish(h, ok)
}

/// `SessionHandler::destroy(string $id): bool`.
pub fn session_handler_destroy(h: &mut dyn SessionHost, id: &[u8]) -> RtResult<bool> {
    let Some(m) = sanity_open(h)? else {
        return Ok(false);
    };
    let ok = mod_destroy(h, &m, id);
    finish(h, ok)
}

/// `SessionHandler::gc(int $max_lifetime): int|false`.
pub fn session_handler_gc(h: &mut dyn SessionHost, max_lifetime: i64) -> RtResult<Value> {
    let Some(m) = sanity_open(h)? else {
        return Ok(Value::Bool(false));
    };
    let n = mod_gc(h, &m, max_lifetime);
    finish(
        h,
        if n == -1 {
            Value::Bool(false)
        } else {
            Value::Int(n)
        },
    )
}

/// `SessionHandler::create_sid(): string`.
pub fn session_handler_create_sid(h: &mut dyn SessionHost) -> RtResult<Value> {
    let m = sanity(h)?;
    if !h.session().mod_user_is_open {
        warn(
            h,
            ErrorLevel::Warning,
            "Parent session handler is not open, defaulting to session_create_id()",
        );
        let id = create_default_id(h).unwrap_or_default();
        return finish(h, Value::Str(ZStr::from(id)));
    }
    if let Some(id) = mod_create_sid(h, &m) {
        finish(h, Value::Str(ZStr::from(id)))
    } else {
        let (name, path) = {
            let s = h.session();
            (
                s.module.as_ref().map(ModRef::name).unwrap_or_default(),
                s.config.save_path.to_string_lossy(),
            )
        };
        throw(
            h,
            RuntimeError::Error(format!(
                "Failed to create session ID: {name} (path: {path})"
            )),
        );
        finish(h, Value::Null)
    }
}

/// `SessionHandler::validateId(string $id): bool`.
pub fn session_handler_validate_id(h: &mut dyn SessionHost, id: &[u8]) -> RtResult<bool> {
    let m = sanity(h)?;
    if !h.session().mod_user_is_open {
        warn(
            h,
            ErrorLevel::Warning,
            "Parent session handler is not open, ignoring ID validation",
        );
        return finish(h, true);
    }
    let ok = mod_validate_sid(h, &m, id);
    finish(h, ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations() {
        assert_eq!(class_table().len(), 4);
        let sh = class_table()
            .iter()
            .find(|c| c.name == "SessionHandler")
            .unwrap();
        assert_eq!(sh.methods.len(), 8);
        assert!(!sh.is_interface);
        assert_eq!(FUNCTIONS.len(), 23);
    }

    #[test]
    fn implementation_warnings() {
        let w = handler_interface_implemented("MissingBoth", false, false, false, false);
        assert_eq!(
            w,
            vec![
                "Class MissingBoth implementing SessionHandlerInterface is missing the \
                 create_sid() method which will be required in PHP 9.0",
                "Class MissingBoth implementing SessionHandlerInterface is missing the \
                 validateId() method which will be required in PHP 9.0"
            ]
        );
        assert!(handler_interface_implemented("X", true, false, false, true).is_empty());
    }
}

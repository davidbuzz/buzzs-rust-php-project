// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `session_*()` functions (`PHP_FUNCTION`s of `session.c`).
//!
//! Each function takes the host and already-parsed arguments; the few
//! whose parameter parsing is part of their semantics
//! (`session_set_save_handler`, `session_set_cookie_params`'s first
//! argument, `session_start`'s options) take raw [`Value`]s. Functions
//! returning `string|false`/`int|false` return a [`Value`]. Exceptions
//! (including those thrown by user save handlers or error handlers while
//! the function runs) come back as `Err`.

use crate::handler::{mod_close, mod_create_sid, mod_destroy, ModRef};
use crate::host::SessionHost;
use crate::ini::{alter, IniStage, INI_USER};
use crate::sid::{valid_key, PS_MAX_SID_LENGTH};
use crate::state::{
    self, already_started_error, clear_session_array, finish, headers_sent_error, name_and_path,
    session_array, throw, warn, SessionStatus,
};
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{ArrayKey, ErrorLevel, RtResult, RuntimeError, Value, ZArray, ZStr};

fn value_err(func: &str, arg: u32, name: &str, msg: &str) -> RuntimeError {
    RuntimeError::ValueError(format!("{func}(): Argument #{arg} (${name}) {msg}"))
}

/// The `P` ZPP check (no NUL bytes in a path-like string).
fn check_path(func: &str, arg: u32, name: &str, v: Option<&ZStr>) -> RtResult<()> {
    if v.is_some_and(|s| s.as_bytes().contains(&0)) {
        return Err(value_err(
            func,
            arg,
            name,
            "must not contain any null bytes",
        ));
    }
    Ok(())
}

fn set_ini(h: &mut dyn SessionHost, name: &str, value: &[u8]) -> bool {
    alter(h, name, value, INI_USER, IniStage::Runtime).unwrap_or(false)
}

/// `php_can_change_session_setting()`.
fn can_change(h: &mut dyn SessionHost, setting: &str, check_cookies: bool) -> bool {
    if h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            &format!("Session {setting} cannot be changed when a session is active"),
        );
        return false;
    }
    if h.headers_sent() && (!check_cookies || h.session().config.use_cookies) {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            &format!("Session {setting} cannot be changed after headers have already been sent"),
        );
        return false;
    }
    true
}

/// `session_set_cookie_params(array|int $lifetime_or_options, ?string $path
/// = null, ?string $domain = null, ?bool $secure = null, ?bool $httponly =
/// null): bool`.
#[allow(clippy::too_many_lines)]
pub fn session_set_cookie_params(
    h: &mut dyn SessionHost,
    lifetime_or_options: &Value,
    path: Option<&ZStr>,
    domain: Option<&ZStr>,
    secure: Option<bool>,
    httponly: Option<bool>,
) -> RtResult<bool> {
    const F: &str = "session_set_cookie_params";
    let arg1 = lifetime_or_options.deref();
    let options = match &arg1 {
        Value::Array(a) => Some(a.clone()),
        Value::Int(_) | Value::Bool(_) | Value::Null => None,
        Value::Float(f) if f.fract() == 0.0 && f.is_finite() => None,
        Value::Str(s)
            if is_numeric_string_ex(s.as_bytes(), false)
                .is_some_and(|n| matches!(n.value, Number::Int(_))) =>
        {
            None
        }
        other => {
            return Err(RuntimeError::TypeError(format!(
                "{F}(): Argument #1 ($lifetime_or_options) must be of type array|int, {} given",
                other.value_name()
            )))
        }
    };
    if !h.session().config.use_cookies {
        warn(
            h,
            ErrorLevel::Warning,
            "Session cookies cannot be used when session.use_cookies is disabled",
        );
        return finish(h, false);
    }
    if h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            "Session cookie parameters cannot be changed when a session is active",
        );
        return finish(h, false);
    }
    if h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session cookie parameters cannot be changed after headers have already been sent",
        );
        return finish(h, false);
    }
    let mut lifetime: Option<ZStr> = None;
    let mut path = path.cloned();
    let mut domain = domain.cloned();
    let mut secure = secure;
    let mut httponly = httponly;
    let mut partitioned: Option<bool> = None;
    let mut samesite: Option<ZStr> = None;
    if let Some(opts) = options {
        let must_be_null = "must be null when argument #1 ($lifetime_or_options) is an array";
        if path.is_some() {
            return Err(value_err(F, 2, "path", must_be_null));
        }
        if domain.is_some() {
            return Err(value_err(F, 3, "domain", must_be_null));
        }
        if secure.is_some() {
            return Err(value_err(F, 4, "secure", must_be_null));
        }
        if httponly.is_some() {
            return Err(value_err(F, 5, "httponly", must_be_null));
        }
        let mut found = 0;
        for (k, v) in opts.iter() {
            let v = v.deref();
            let ArrayKey::Str(key) = k else {
                warn(
                    h,
                    ErrorLevel::Warning,
                    "Argument #1 ($lifetime_or_options) cannot contain numeric keys",
                );
                continue;
            };
            let kb = key.as_bytes();
            let is = |n: &str| kb.eq_ignore_ascii_case(n.as_bytes());
            if is("lifetime") {
                lifetime = Some(v.to_zstr(h)?);
            } else if is("path") {
                path = Some(v.to_zstr(h)?);
            } else if is("domain") {
                domain = Some(v.to_zstr(h)?);
            } else if is("secure") {
                secure = Some(v.to_bool(h)?);
            } else if is("partitioned") {
                partitioned = Some(v.to_bool(h)?);
            } else if is("httponly") {
                httponly = Some(v.to_bool(h)?);
            } else if is("samesite") {
                samesite = Some(v.to_zstr(h)?);
            } else {
                warn(
                    h,
                    ErrorLevel::Warning,
                    &format!(
                        "Argument #1 ($lifetime_or_options) contains an unrecognized key \"{}\"",
                        key.to_string_lossy()
                    ),
                );
                continue;
            }
            found += 1;
        }
        if found == 0 {
            if let Some(e) = h.session().take_exception() {
                return Err(e);
            }
            return Err(value_err(
                F,
                1,
                "lifetime_or_options",
                "must contain at least 1 valid key",
            ));
        }
    } else {
        let n = match &arg1 {
            Value::Int(n) => *n,
            other => other.to_long(h)?,
        };
        lifetime = Some(ZStr::from(n.to_string()));
    }
    if h.session().has_exception() {
        return finish(h, false);
    }
    let bool_str = |b: bool| if b { &b"1"[..] } else { &b"0"[..] };
    let steps: [(&str, Option<Vec<u8>>); 7] = [
        (
            "session.cookie_lifetime",
            lifetime.map(|s| s.as_bytes().to_vec()),
        ),
        ("session.cookie_path", path.map(|s| s.as_bytes().to_vec())),
        (
            "session.cookie_domain",
            domain.map(|s| s.as_bytes().to_vec()),
        ),
        (
            "session.cookie_secure",
            secure.map(|b| bool_str(b).to_vec()),
        ),
        (
            "session.cookie_partitioned",
            partitioned.map(|b| bool_str(b).to_vec()),
        ),
        (
            "session.cookie_httponly",
            httponly.map(|b| bool_str(b).to_vec()),
        ),
        (
            "session.cookie_samesite",
            samesite.map(|s| s.as_bytes().to_vec()),
        ),
    ];
    for (name, value) in steps {
        if let Some(v) = value {
            if !set_ini(h, name, &v) {
                return finish(h, false);
            }
        }
    }
    finish(h, true)
}

/// `session_get_cookie_params(): array`.
pub fn session_get_cookie_params(h: &mut dyn SessionHost) -> ZArray {
    let c = &h.session().config;
    let mut a = ZArray::new();
    let t = a.make_mut();
    t.insert("lifetime", Value::Int(c.cookie_lifetime));
    t.insert("path", Value::Str(c.cookie_path.clone()));
    t.insert("domain", Value::Str(c.cookie_domain.clone()));
    t.insert("secure", Value::Bool(c.cookie_secure));
    t.insert("partitioned", Value::Bool(c.cookie_partitioned));
    t.insert("httponly", Value::Bool(c.cookie_httponly));
    t.insert("samesite", Value::Str(c.cookie_samesite.clone()));
    a
}

/// `session_name(?string $name = null): string|false`.
pub fn session_name(h: &mut dyn SessionHost, name: Option<&ZStr>) -> RtResult<Value> {
    check_path("session_name", 1, "name", name)?;
    if name.is_some() && !can_change(h, "name", false) {
        return finish(h, Value::Bool(false));
    }
    let old = h.session().config.session_name.clone();
    if let Some(n) = name {
        set_ini(h, "session.name", n.as_bytes());
    }
    finish(h, Value::Str(old))
}

/// `session_module_name(?string $module = null): string|false`.
pub fn session_module_name(h: &mut dyn SessionHost, module: Option<&ZStr>) -> RtResult<Value> {
    check_path("session_module_name", 1, "module", module)?;
    if module.is_some() && !can_change(h, "save handler module", false) {
        return finish(h, Value::Bool(false));
    }
    let old = h.session().module_name().unwrap_or_default();
    if let Some(m) = module {
        if m.as_bytes().eq_ignore_ascii_case(b"user") {
            return Err(value_err(
                "session_module_name",
                1,
                "module",
                "must not be \"user\"",
            ));
        }
        if h.session().find_module(m.as_bytes()).is_none() {
            warn(
                h,
                ErrorLevel::Warning,
                &format!(
                    "Session handler module \"{}\" cannot be found",
                    m.to_string_lossy()
                ),
            );
            return finish(h, Value::Bool(false));
        }
        let open = h.session().mod_data.is_some() || h.session().mod_user_implemented;
        if open {
            if let Some(cur) = h.session().module.clone() {
                mod_close(h, &cur);
            }
        }
        h.session().mod_data = None;
        set_ini(h, "session.save_handler", m.as_bytes());
    }
    finish(h, Value::string(old))
}

/// `can_session_handler_be_changed()`.
fn handler_changeable(h: &mut dyn SessionHost) -> bool {
    if h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            "Session save handler cannot be changed when a session is active",
        );
        return false;
    }
    if h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session save handler cannot be changed after headers have already been sent",
        );
        return false;
    }
    true
}

/// `set_user_save_handler_ini()`.
fn set_user_handler_ini(h: &mut dyn SessionHost) {
    h.session().set_handler = true;
    set_ini(h, "session.save_handler", b"user");
    h.session().set_handler = false;
}

fn method(obj: &Value, name: &str) -> Value {
    Value::Array(ZArray::from_values([obj.clone(), Value::string(name)]))
}

/// The `b` ZPP specifier in coercive mode.
fn zpp_bool(
    h: &mut dyn SessionHost,
    func: &str,
    arg: u32,
    name: &str,
    v: &Value,
) -> RtResult<bool> {
    match v.deref() {
        Value::Bool(b) => Ok(b),
        v @ (Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Null) => v.to_bool(h),
        other => Err(RuntimeError::TypeError(format!(
            "{func}(): Argument #{arg} (${name}) must be of type bool, {} given",
            other.value_name()
        ))),
    }
}

/// `session_set_save_handler(...)`: both the object signature
/// (`SessionHandlerInterface $sessionhandler, bool $register_shutdown =
/// true`) and the deprecated callback signature (6 to 9 callables).
#[allow(clippy::too_many_lines)]
pub fn session_set_save_handler(h: &mut dyn SessionHost, args: &[Value]) -> RtResult<bool> {
    const F: &str = "session_set_save_handler";
    const NAMES: [&str; 9] = [
        "open",
        "close",
        "read",
        "write",
        "destroy",
        "gc",
        "create_sid",
        "validate_sid",
        "update_timestamp",
    ];
    if args.len() <= 2 {
        let Some(first) = args.first() else {
            return Err(RuntimeError::ArgumentCountError(format!(
                "{F}() expects at least 1 argument, 0 given"
            )));
        };
        let obj = first.deref();
        let o = match &obj {
            Value::Object(o) if h.instance_of(o, "SessionHandlerInterface") => o.clone(),
            other => {
                return Err(RuntimeError::TypeError(format!(
                    "{F}(): Argument #1 ($open) must be of type SessionHandlerInterface, {} given",
                    other.value_name()
                )))
            }
        };
        let register_shutdown = match args.get(1) {
            Some(v) => zpp_bool(h, F, 2, "close", v)?,
            None => true,
        };
        if !handler_changeable(h) {
            return finish(h, false);
        }
        {
            let u = &mut h.session().user;
            u.open = Some(method(&obj, "open"));
            u.close = Some(method(&obj, "close"));
            u.read = Some(method(&obj, "read"));
            u.write = Some(method(&obj, "write"));
            u.destroy = Some(method(&obj, "destroy"));
            u.gc = Some(method(&obj, "gc"));
            u.create_sid = None;
        }
        if h.instance_of(&o, "SessionIdInterface") || h.has_method(&o, "create_sid") {
            h.session().user.create_sid = Some(method(&obj, "create_sid"));
        } else {
            warn(
                h,
                ErrorLevel::Deprecated,
                "Providing an object to argument #1 ($sessionhandler) which does not have the \
                 create_sid() method defined is deprecated",
            );
        }
        {
            let u = &mut h.session().user;
            u.validate_sid = None;
            u.update_timestamp = None;
        }
        if h.instance_of(&o, "SessionUpdateTimestampHandlerInterface") {
            let u = &mut h.session().user;
            u.validate_sid = Some(method(&obj, "validateid"));
            u.update_timestamp = Some(method(&obj, "updateTimestamp"));
        } else {
            if h.has_method(&o, "validateid") {
                h.session().user.validate_sid = Some(method(&obj, "validateid"));
            } else {
                warn(
                    h,
                    ErrorLevel::Deprecated,
                    "Providing an object to argument #1 ($sessionhandler) which does not have \
                     the validateId() method defined is deprecated",
                );
            }
            if h.has_method(&o, "updatetimestamp") {
                h.session().user.update_timestamp = Some(method(&obj, "updateTimestamp"));
            }
        }
        h.session().mod_user_uses_object_methods_as_handlers = true;
        if register_shutdown {
            if !h.register_session_shutdown() {
                warn(
                    h,
                    ErrorLevel::Warning,
                    "Unable to register session shutdown function",
                );
                return finish(h, false);
            }
        } else {
            h.remove_session_shutdown();
        }
        let is_user = h.session().module.as_ref().is_some_and(ModRef::is_user);
        if h.session().status != SessionStatus::Active && !is_user {
            set_user_handler_ini(h);
        }
        return finish(h, true);
    }

    warn(
        h,
        ErrorLevel::Deprecated,
        "Providing individual callbacks instead of an object implementing \
         SessionHandlerInterface is deprecated",
    );
    if let Some(e) = h.session().take_exception() {
        return Err(e);
    }
    if args.len() < 6 {
        return Err(RuntimeError::ArgumentCountError(format!(
            "{F}() expects at least 6 arguments, {} given",
            args.len()
        )));
    }
    if args.len() > 9 {
        return Err(RuntimeError::ArgumentCountError(format!(
            "{F}() expects at most 9 arguments, {} given",
            args.len()
        )));
    }
    let mut cbs: Vec<Option<Value>> = Vec::with_capacity(9);
    for (i, v) in args.iter().enumerate() {
        if i >= 6 && v.is_null() {
            cbs.push(None);
            continue;
        }
        if let Err(msg) = h.check_callable(v) {
            return Err(RuntimeError::TypeError(format!(
                "{F}(): Argument #{} (${}) must be a valid callback{}, {msg}",
                i + 1,
                NAMES[i],
                if i >= 6 { " or null" } else { "" }
            )));
        }
        cbs.push(Some(v.clone()));
    }
    if !handler_changeable(h) {
        return finish(h, false);
    }
    h.remove_session_shutdown();
    let is_user = h.session().module.as_ref().is_some_and(ModRef::is_user);
    if !is_user {
        set_user_handler_ini(h);
    }
    let mut it = cbs.into_iter();
    let u = &mut h.session().user;
    u.open = it.next().flatten();
    u.close = it.next().flatten();
    u.read = it.next().flatten();
    u.write = it.next().flatten();
    u.destroy = it.next().flatten();
    u.gc = it.next().flatten();
    if let Some(Some(v)) = it.next() {
        u.create_sid = Some(v);
    }
    if let Some(Some(v)) = it.next() {
        u.validate_sid = Some(v);
    }
    if let Some(Some(v)) = it.next() {
        u.update_timestamp = Some(v);
    }
    h.session().mod_user_uses_object_methods_as_handlers = false;
    finish(h, true)
}

/// `session_save_path(?string $path = null): string|false`.
pub fn session_save_path(h: &mut dyn SessionHost, path: Option<&ZStr>) -> RtResult<Value> {
    check_path("session_save_path", 1, "path", path)?;
    if path.is_some() && !can_change(h, "save path", false) {
        return finish(h, Value::Bool(false));
    }
    let old = h.session().config.save_path.clone();
    if let Some(p) = path {
        set_ini(h, "session.save_path", p.as_bytes());
    }
    finish(h, Value::Str(old))
}

/// `session_id(?string $id = null): string|false`.
pub fn session_id(h: &mut dyn SessionHost, id: Option<&ZStr>) -> RtResult<Value> {
    if id.is_some() && !can_change(h, "ID", true) {
        return finish(h, Value::Bool(false));
    }
    let old = match &h.session().id {
        Some(cur) => {
            let b = cur.as_bytes();
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            ZStr::from(&b[..end])
        }
        None => ZStr::empty(),
    };
    if let Some(new) = id {
        h.session().id = Some(new.clone());
    }
    finish(h, Value::Str(old))
}

/// `session_regenerate_id(bool $delete_old_session = false): bool`.
#[allow(clippy::too_many_lines)]
pub fn session_regenerate_id(h: &mut dyn SessionHost, delete_old_session: bool) -> RtResult<bool> {
    if h.session().status != SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            "Session ID cannot be regenerated when there is no active session",
        );
        return finish(h, false);
    }
    if h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session ID cannot be regenerated after headers have already been sent",
        );
        return finish(h, false);
    }
    let Some(m) = h.session().module.clone() else {
        return finish(h, false);
    };
    let id = h.session().id.clone().unwrap_or_default();
    if delete_old_session {
        if !mod_destroy(h, &m, id.as_bytes()) {
            mod_close(h, &m);
            h.session().status = SessionStatus::None;
            if !h.session().has_exception() {
                let (name, path) = name_and_path(h);
                warn(
                    h,
                    ErrorLevel::Warning,
                    &format!("Session object destruction failed. ID: {name} (path: {path})"),
                );
            }
            return finish(h, false);
        }
    } else {
        let Some(data) = state::encode(h) else {
            mod_close(h, &m);
            h.session().status = SessionStatus::None;
            return finish(h, false);
        };
        if !state::write_with(h, &m, id.as_bytes(), &data) {
            mod_close(h, &m);
            h.session().status = SessionStatus::None;
            let (name, path) = name_and_path(h);
            warn(
                h,
                ErrorLevel::Warning,
                &format!("Session write failed. ID: {name} (path: {path})"),
            );
            return finish(h, false);
        }
    }
    mod_close(h, &m);
    if h.session().status != SessionStatus::Active {
        warn(
            h,
            ErrorLevel::Warning,
            "Session ID cannot be regenerated because the save handler closed the session",
        );
        return finish(h, false);
    }
    h.session().session_vars = None;
    h.session().id = None;
    if !state::open_with(h, &m) {
        h.session().status = SessionStatus::None;
        if !h.session().has_exception() {
            let (name, path) = name_and_path(h);
            throw(
                h,
                RuntimeError::Error(format!("Failed to open session: {name} (path: {path})")),
            );
        }
        return finish(h, false);
    }
    let Some(new_id) = mod_create_sid(h, &m) else {
        h.session().status = SessionStatus::None;
        if !h.session().has_exception() {
            let (name, path) = name_and_path(h);
            throw(
                h,
                RuntimeError::Error(format!(
                    "Failed to create new session ID: {name} (path: {path})"
                )),
            );
        }
        return finish(h, false);
    };
    h.session().id = Some(ZStr::from(new_id));
    if h.session().config.use_strict_mode {
        let can_validate = {
            let s = h.session();
            !s.mod_user_implemented || s.user.validate_sid.is_some()
        };
        if can_validate {
            let mut limit = 3;
            while limit > 0 {
                limit -= 1;
                let cur = h.session().id.clone().unwrap_or_default();
                if !state::validate(h, &m, cur.as_bytes()) {
                    break;
                }
                h.session().id = None;
                if let Some(id) = mod_create_sid(h, &m) {
                    h.session().id = Some(ZStr::from(id));
                } else {
                    mod_close(h, &m);
                    h.session().status = SessionStatus::None;
                    if !h.session().has_exception() {
                        let (name, path) = name_and_path(h);
                        throw(
                            h,
                            RuntimeError::Error(format!(
                                "Failed to create session ID by collision: {name} (path: {path})"
                            )),
                        );
                    }
                    return finish(h, false);
                }
            }
        }
    }
    let cur = h.session().id.clone().unwrap_or_default();
    if state::read_with(h, &m, cur.as_bytes()).is_none() {
        mod_close(h, &m);
        h.session().status = SessionStatus::None;
        if !h.session().has_exception() {
            let (name, path) = name_and_path(h);
            throw(
                h,
                RuntimeError::Error(format!(
                    "Failed to create(read) session ID: {name} (path: {path})"
                )),
            );
        }
        return finish(h, false);
    }
    if h.session().config.use_cookies {
        h.session().send_cookie = true;
    }
    let ok = state::reset_id(h);
    finish(h, ok)
}

/// `session_create_id(string $prefix = ""): string|false`.
pub fn session_create_id(h: &mut dyn SessionHost, prefix: Option<&ZStr>) -> RtResult<Value> {
    const F: &str = "session_create_id";
    check_path(F, 1, "prefix", prefix)?;
    let mut id = Vec::new();
    if let Some(p) = prefix.filter(|p| !p.is_empty()) {
        if p.len() > PS_MAX_SID_LENGTH {
            return Err(value_err(
                F,
                1,
                "prefix",
                &format!("cannot be longer than {PS_MAX_SID_LENGTH} characters"),
            ));
        }
        if !valid_key(p.as_bytes()) {
            warn(
                h,
                ErrorLevel::Warning,
                "Prefix cannot contain special characters. Only the A-Z, a-z, 0-9, \"-\", and \
                 \",\" characters are allowed",
            );
            return finish(h, Value::Bool(false));
        }
        id.extend_from_slice(p.as_bytes());
    }
    let active = {
        let s = h.session();
        !s.in_save_handler && s.status == SessionStatus::Active
    };
    let new_id = if active {
        let m = h.session().module.clone();
        let mut new_id = None;
        if let Some(m) = m {
            for _ in 0..3 {
                new_id = mod_create_sid(h, &m);
                let Some(candidate) = new_id.clone() else {
                    break;
                };
                let no_validation = {
                    let s = h.session();
                    s.mod_user_implemented && s.user.validate_sid.is_none()
                };
                if no_validation || !state::validate(h, &m, &candidate) {
                    break;
                }
                new_id = None;
            }
        }
        new_id
    } else {
        state::create_default_id(h)
    };
    if let Some(n) = new_id {
        id.extend_from_slice(&n);
        finish(h, Value::Str(ZStr::from(id)))
    } else {
        if let Some(e) = h.session().take_exception() {
            return Err(e);
        }
        warn(h, ErrorLevel::Warning, "Failed to create new ID");
        finish(h, Value::Bool(false))
    }
}

/// `session_cache_limiter(?string $value = null): string|false`.
pub fn session_cache_limiter(h: &mut dyn SessionHost, value: Option<&ZStr>) -> RtResult<Value> {
    if value.is_some() && h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            "Session cache limiter cannot be changed when a session is active",
        );
        return finish(h, Value::Bool(false));
    }
    if value.is_some() && h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session cache limiter cannot be changed after headers have already been sent",
        );
        return finish(h, Value::Bool(false));
    }
    let old = h.session().config.cache_limiter.clone();
    if let Some(v) = value {
        set_ini(h, "session.cache_limiter", v.as_bytes());
    }
    finish(h, Value::Str(old))
}

/// `session_cache_expire(?int $value = null): int|false`.
pub fn session_cache_expire(h: &mut dyn SessionHost, value: Option<i64>) -> RtResult<Value> {
    let old = h.session().config.cache_expire;
    if value.is_some() && h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            "Session cache expiration cannot be changed when a session is active",
        );
        return finish(h, Value::Int(old));
    }
    if value.is_some() && h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session cache expiration cannot be changed after headers have already been sent",
        );
        return finish(h, Value::Bool(false));
    }
    if let Some(v) = value {
        set_ini(h, "session.cache_expire", v.to_string().as_bytes());
    }
    finish(h, Value::Int(old))
}

/// `session_encode(): string|false`.
pub fn session_encode(h: &mut dyn SessionHost) -> RtResult<Value> {
    // `php_session_encode()`: `IF_SESSION_VARS()`, not the session status,
    // so `$_SESSION` still encodes after `session_write_close()`.
    if state::session_array(h).is_none() {
        warn(h, ErrorLevel::Warning, "Cannot encode non-existent session");
        return finish(h, Value::Bool(false));
    }
    let v = match state::encode(h) {
        Some(data) => Value::Str(ZStr::from(data)),
        None => Value::Bool(false),
    };
    finish(h, v)
}

/// `session_decode(string $data): bool`.
pub fn session_decode(h: &mut dyn SessionHost, data: &[u8]) -> RtResult<bool> {
    if h.session().status != SessionStatus::Active {
        warn(
            h,
            ErrorLevel::Warning,
            "Session data cannot be decoded when there is no active session",
        );
        return finish(h, false);
    }
    let ok = state::decode(h, data);
    finish(h, ok)
}

/// `session_start(array $options = []): bool`.
#[allow(clippy::many_single_char_names)]
pub fn session_start(h: &mut dyn SessionHost, options: Option<&ZArray>) -> RtResult<bool> {
    const F: &str = "session_start";
    if h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Notice,
            "Ignoring session_start() because a session is already active",
        );
        return finish(h, true);
    }
    if h.session().config.use_cookies && h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session cannot be started after headers have already been sent",
        );
        return finish(h, false);
    }
    let mut read_and_close = false;
    if let Some(opts) = options {
        for (k, v) in opts.iter() {
            let ArrayKey::Str(key) = k else {
                return Err(value_err(
                    F,
                    1,
                    "options",
                    "must be of type array with keys as string",
                ));
            };
            let v = v.deref();
            match &v {
                Value::Str(_) | Value::Bool(_) | Value::Int(_) => {
                    if key.as_bytes() == b"read_and_close" {
                        let n = match &v {
                            Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
                                Some(n)
                                    if !n.trailing_data && matches!(n.value, Number::Int(_)) =>
                                {
                                    match n.value {
                                        Number::Int(i) => i,
                                        Number::Float(_) => 0,
                                    }
                                }
                                _ => {
                                    return Err(RuntimeError::TypeError(format!(
                                        "{F}(): Option \"{}\" value must be of type compatible \
                                         with int, \"{}\" given",
                                        key.to_string_lossy(),
                                        s.to_string_lossy()
                                    )))
                                }
                            },
                            Value::Bool(b) => i64::from(*b),
                            Value::Int(i) => *i,
                            _ => 0,
                        };
                        read_and_close = n > 0;
                    } else {
                        let val = v.to_zstr(h)?;
                        let mut name = b"session.".to_vec();
                        name.extend_from_slice(key.as_bytes());
                        let name = String::from_utf8_lossy(&name).into_owned();
                        let ok = match alter(h, &name, val.as_bytes(), INI_USER, IniStage::Runtime)
                        {
                            Some(ok) => ok,
                            None => h.alter_foreign_ini(&name, val.as_bytes()),
                        };
                        if !ok {
                            warn(
                                h,
                                ErrorLevel::Warning,
                                &format!("Setting option \"{}\" failed", key.to_string_lossy()),
                            );
                        }
                    }
                }
                other => {
                    return Err(RuntimeError::TypeError(format!(
                        "{F}(): Option \"{}\" must be of type string|int|bool, {} given",
                        key.to_string_lossy(),
                        other.value_name()
                    )))
                }
            }
        }
    }
    state::start(h);
    if h.session().status != SessionStatus::Active {
        clear_session_array(h);
        return finish(h, false);
    }
    if read_and_close {
        state::flush(h, false);
    }
    finish(h, true)
}

/// `session_destroy(): bool`.
pub fn session_destroy(h: &mut dyn SessionHost) -> RtResult<bool> {
    let ok = state::destroy(h);
    finish(h, ok)
}

/// `session_unset(): bool`.
pub fn session_unset(h: &mut dyn SessionHost) -> RtResult<bool> {
    if h.session().status != SessionStatus::Active {
        return finish(h, false);
    }
    clear_session_array(h);
    finish(h, true)
}

/// `session_gc(): int|false`.
pub fn session_gc(h: &mut dyn SessionHost) -> RtResult<Value> {
    if h.session().status != SessionStatus::Active {
        warn(
            h,
            ErrorLevel::Warning,
            "Session cannot be garbage collected when there is no active session",
        );
        return finish(h, Value::Bool(false));
    }
    let n = state::gc(h, true);
    finish(
        h,
        if n < 0 {
            Value::Bool(false)
        } else {
            Value::Int(n)
        },
    )
}

/// `session_write_close(): bool` (alias `session_commit`).
pub fn session_write_close(h: &mut dyn SessionHost) -> RtResult<bool> {
    let ok = state::flush(h, true);
    finish(h, ok)
}

/// `session_commit(): bool`.
pub fn session_commit(h: &mut dyn SessionHost) -> RtResult<bool> {
    session_write_close(h)
}

/// `session_abort(): bool`.
pub fn session_abort(h: &mut dyn SessionHost) -> RtResult<bool> {
    let ok = state::abort(h);
    finish(h, ok)
}

/// `session_reset(): bool`.
pub fn session_reset(h: &mut dyn SessionHost) -> RtResult<bool> {
    let ok = state::reset(h);
    finish(h, ok)
}

/// `session_status(): int`.
pub fn session_status(h: &mut dyn SessionHost) -> i64 {
    h.session().status.as_int()
}

/// `session_register_shutdown(): void`.
pub fn session_register_shutdown(h: &mut dyn SessionHost) -> RtResult<()> {
    if !h.append_write_close_shutdown() {
        state::flush(h, true);
        warn(
            h,
            ErrorLevel::Warning,
            "Session shutdown function cannot be registered",
        );
    }
    finish(h, ())
}

/// Whether `$_SESSION` currently holds an array (`IF_SESSION_VARS`).
pub fn has_session_vars(h: &mut dyn SessionHost) -> bool {
    session_array(h).is_some()
}

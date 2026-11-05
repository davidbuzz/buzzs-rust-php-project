//! The per-request session state (`php_ps_globals`) and the internal
//! lifecycle of `session.c`: `php_session_start`, `php_session_initialize`,
//! `php_session_reset_id`, `php_session_send_cookie`, cache limiters,
//! `php_session_save_current_state`, GC, destroy/abort/reset and the
//! request startup/shutdown hooks.

use crate::files::FilesModule;
use crate::handler::{
    mod_close, mod_create_sid, mod_destroy, mod_gc, mod_has_update_timestamp, mod_open, mod_read,
    mod_update_timestamp, mod_validate_sid, mod_write, ModRef, SaveHandler, SaveModule,
};
use crate::host::SessionHost;
use crate::http::{session_cookie, CacheLimiter, CookieParams};
use crate::ini::{IniStore, SessionConfig};
use crate::serializer::{self, Decoded, Serializer};
use crate::sid;
use crate::user::{Slot, UserHandlers};
use php_runtime::{ErrorLevel, PhpRef, RtResult, RuntimeError, Value, ZArray, ZStr};
use std::rc::Rc;

/// `MAX_MODULES`.
const MAX_MODULES: usize = 32;

/// `php_session_status` (the values of the `PHP_SESSION_*` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionStatus {
    /// `PHP_SESSION_DISABLED` (0): no usable save handler/serializer.
    Disabled,
    /// `PHP_SESSION_NONE` (1).
    #[default]
    None,
    /// `PHP_SESSION_ACTIVE` (2).
    Active,
}

impl SessionStatus {
    /// The integer `session_status()` returns.
    #[must_use]
    pub fn as_int(self) -> i64 {
        match self {
            SessionStatus::Disabled => 0,
            SessionStatus::None => 1,
            SessionStatus::Active => 2,
        }
    }
}

/// The per-request session state (`PS()` globals).
#[allow(clippy::struct_excessive_bools)] // mirrors `php_ps_globals`
#[derive(Debug)]
pub struct SessionState {
    /// Typed INI values.
    pub config: SessionConfig,
    /// Raw INI values.
    pub ini: IniStore,
    pub(crate) status: SessionStatus,
    pub(crate) id: Option<ZStr>,
    pub(crate) module: Option<ModRef>,
    pub(crate) default_mod: Option<ModRef>,
    pub(crate) serializer: Option<Serializer>,
    pub(crate) mod_data: Option<Box<dyn SaveHandler>>,
    pub(crate) user: UserHandlers,
    pub(crate) mod_user_implemented: bool,
    pub(crate) mod_user_is_open: bool,
    pub(crate) mod_user_uses_object_methods_as_handlers: bool,
    pub(crate) in_save_handler: bool,
    pub(crate) set_handler: bool,
    pub(crate) define_sid: bool,
    pub(crate) send_cookie: bool,
    pub(crate) session_vars: Option<Vec<u8>>,
    pub(crate) http_session_vars: Option<PhpRef>,
    pub(crate) started_at: Option<(String, u32)>,
    pub(crate) exception: Option<RuntimeError>,
    pub(crate) modules: Vec<Rc<dyn SaveModule>>,
    pub(crate) modules_activated: bool,
    pub(crate) in_rfc1867_update: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    /// A fresh state with built-in defaults and the `files` module
    /// registered (the result of `MINIT` without configuration).
    #[must_use]
    pub fn new() -> Self {
        let files: Rc<dyn SaveModule> = Rc::new(FilesModule);
        Self {
            config: SessionConfig::default(),
            ini: IniStore::default(),
            status: SessionStatus::None,
            id: None,
            module: Some(ModRef::Native(files.clone())),
            default_mod: None,
            serializer: Some(Serializer::Php),
            mod_data: None,
            user: UserHandlers::default(),
            mod_user_implemented: false,
            mod_user_is_open: false,
            mod_user_uses_object_methods_as_handlers: false,
            in_save_handler: false,
            set_handler: false,
            define_sid: true,
            send_cookie: false,
            session_vars: None,
            http_session_vars: None,
            started_at: None,
            exception: None,
            modules: vec![files],
            modules_activated: true,
            in_rfc1867_update: false,
        }
    }

    /// `PS(session_status)`.
    #[must_use]
    pub fn status(&self) -> SessionStatus {
        self.status
    }

    /// `PS(id)`.
    #[must_use]
    pub fn id(&self) -> Option<&ZStr> {
        self.id.as_ref()
    }

    /// The current save handler module name (`PS(mod)->s_name`).
    #[must_use]
    pub fn module_name(&self) -> Option<String> {
        self.module.as_ref().map(ModRef::name)
    }

    /// The current serializer.
    #[must_use]
    pub fn serializer(&self) -> Option<Serializer> {
        self.serializer
    }

    /// The registered user save handler callables.
    #[must_use]
    pub fn user_handlers(&self) -> &UserHandlers {
        &self.user
    }

    /// Whether the user handlers came from an object
    /// (`PS(mod_user_uses_object_methods_as_handlers)`).
    #[must_use]
    pub fn uses_object_handlers(&self) -> bool {
        self.mod_user_uses_object_methods_as_handlers
    }

    /// The reference behind `$_SESSION` (`PS(http_session_vars)`).
    #[must_use]
    pub fn session_vars_ref(&self) -> Option<&PhpRef> {
        self.http_session_vars.as_ref()
    }

    /// The lazy-write baseline (the data read at start).
    #[must_use]
    pub fn read_data(&self) -> Option<&[u8]> {
        self.session_vars.as_deref()
    }

    /// Where the active session was started.
    #[must_use]
    pub fn started_at(&self) -> Option<&(String, u32)> {
        self.started_at.as_ref()
    }

    /// `php_session_register_module()`: registers a native module (at most
    /// 32 modules including `files` and `user`).
    pub fn register_module(&mut self, module: Rc<dyn SaveModule>) -> bool {
        if self.modules.len() + 1 >= MAX_MODULES {
            return false;
        }
        self.modules.push(module);
        true
    }

    /// `_php_find_ps_module()` (case-insensitive; `"user"` is built in).
    #[must_use]
    pub fn find_module(&self, name: &[u8]) -> Option<ModRef> {
        let name = name.split(|&c| c == 0).next().unwrap_or_default();
        if let Some(m) = self
            .modules
            .iter()
            .find(|m| m.name().as_bytes().eq_ignore_ascii_case(name))
        {
            return Some(ModRef::Native(m.clone()));
        }
        name.eq_ignore_ascii_case(b"user").then_some(ModRef::User)
    }

    /// The names of all registered save handlers (for `phpinfo()`).
    #[must_use]
    pub fn module_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.modules.iter().map(|m| m.name().to_string()).collect();
        v.insert(1.min(v.len()), "user".to_string());
        v
    }

    /// `PS(in_rfc1867_update)`: while true the session is decoded and
    /// re-encoded during request startup (upload progress), so the host's
    /// (un)serialize hooks must keep objects as `__PHP_Incomplete_Class`
    /// without autoloading or calling `__wakeup`/`__unserialize`.
    #[must_use]
    pub fn in_rfc1867_update(&self) -> bool {
        self.in_rfc1867_update
    }

    /// `PG(modules_activated)`: whether request startup has completed.
    pub fn set_modules_activated(&mut self, on: bool) {
        self.modules_activated = on;
    }

    /// Records a thrown exception (the first one wins, like `EG(exception)`).
    pub fn set_exception(&mut self, e: RuntimeError) {
        if self.exception.is_none() {
            self.exception = Some(e);
        }
    }

    /// Whether an exception is pending.
    #[must_use]
    pub fn has_exception(&self) -> bool {
        self.exception.is_some()
    }

    /// Takes the pending exception.
    pub fn take_exception(&mut self) -> Option<RuntimeError> {
        self.exception.take()
    }
}

// ---------------------------------------------------------------------------
// Diagnostics helpers
// ---------------------------------------------------------------------------

/// `php_error_docref(NULL, level, msg)`; an exception thrown by the error
/// handler becomes pending.
pub(crate) fn warn(h: &mut dyn SessionHost, level: ErrorLevel, msg: &str) {
    if let Err(e) = h.emit_diagnostic(level, msg) {
        h.session().set_exception(e);
    }
}

/// Throws (records) an exception unless one is pending.
pub(crate) fn throw(h: &mut dyn SessionHost, e: RuntimeError) {
    h.session().set_exception(e);
}

/// Returns `v`, or the pending exception.
pub(crate) fn finish<T>(h: &mut dyn SessionHost, v: T) -> RtResult<T> {
    match h.session().take_exception() {
        Some(e) => Err(e),
        None => Ok(v),
    }
}

/// `php_session_session_already_started_error()`.
pub(crate) fn already_started_error(h: &mut dyn SessionHost, level: ErrorLevel, msg: &str) {
    let s = h.session();
    let text = if let Some((file, line)) = &s.started_at {
        format!("{msg} (started from {file} on line {line})")
    } else if s.config.auto_start {
        format!("{msg} (session started automatically)")
    } else {
        msg.to_string()
    };
    warn(h, level, &text);
}

/// `php_session_headers_already_sent_error()`.
pub(crate) fn headers_sent_error(h: &mut dyn SessionHost, level: ErrorLevel, msg: &str) {
    let text = match h.output_start() {
        Some((file, line)) => format!("{msg} (sent from {file} on line {line})"),
        None => msg.to_string(),
    };
    warn(h, level, &text);
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `PS(mod)->s_name` and `PS(save_path)` for messages.
fn mod_name_and_path(h: &mut dyn SessionHost) -> (String, String) {
    let s = h.session();
    (
        s.module.as_ref().map(ModRef::name).unwrap_or_default(),
        s.config.save_path.to_string_lossy(),
    )
}

// ---------------------------------------------------------------------------
// $_SESSION
// ---------------------------------------------------------------------------

/// `php_session_track_init()`: binds a fresh empty `$_SESSION`.
pub(crate) fn track_init(h: &mut dyn SessionHost) {
    let r = PhpRef::new(Value::empty_array());
    h.session().http_session_vars = Some(r.clone());
    h.bind_session_global(Some(r));
}

/// `IF_SESSION_VARS()`: the current `$_SESSION` array.
pub(crate) fn session_array(h: &mut dyn SessionHost) -> Option<ZArray> {
    let r = h.session().http_session_vars.clone()?;
    match r.get() {
        Value::Array(a) => Some(a),
        _ => None,
    }
}

/// Clears `$_SESSION` in place (`zend_hash_clean`).
pub(crate) fn clear_session_array(h: &mut dyn SessionHost) {
    if let Some(r) = h.session().http_session_vars.clone() {
        let mut v = r.borrow_mut();
        if let Some(t) = v.as_array_mut() {
            t.clear();
        }
    }
}

/// `php_session_encode()`.
pub(crate) fn encode(h: &mut dyn SessionHost) -> Option<Vec<u8>> {
    let Some(vars) = session_array(h) else {
        warn(h, ErrorLevel::Warning, "Cannot encode non-existent session");
        return None;
    };
    let ser = h.session().serializer?;
    match serializer::encode(ser, &vars, h) {
        Ok(enc) => {
            for w in &enc.warnings {
                warn(h, ErrorLevel::Warning, w);
            }
            if let Some(e) = enc.exception {
                throw(h, e);
            }
            enc.data
        }
        Err(e) => {
            throw(h, e);
            None
        }
    }
}

/// `php_session_decode()`; on failure the session is destroyed.
pub(crate) fn decode(h: &mut dyn SessionHost, data: &[u8]) -> bool {
    let Some(ser) = h.session().serializer else {
        return false;
    };
    let shared = if h.session().in_rfc1867_update {
        None
    } else {
        serializer::decode_nested(ser, data, &mut |d: &[u8], off: usize| {
            h.unserialize_shared(d, off)
        })
    };
    #[allow(clippy::single_match_else)] // the None arm is a multi-line computation
    let decoded = match shared {
        Some(r) => r,
        None => {
            let opts = h.unserialize_options();
            serializer::decode_with(ser, data, &opts, h)
        }
    };
    let ok = match decoded {
        Ok(Decoded::Merge { entries, ok }) => {
            if let Some(r) = h.session().http_session_vars.clone() {
                let mut v = r.borrow_mut();
                if let Some(t) = v.as_array_mut() {
                    for (name, val) in entries {
                        t.insert_str_raw(name, val);
                    }
                }
            }
            ok
        }
        Ok(Decoded::Replace { value, ok }) => {
            // `if (Z_TYPE(session_vars) == IS_NULL) array_init(...)`
            let value = if value.is_null() {
                Value::empty_array()
            } else {
                value
            };
            let r = PhpRef::new(value);
            h.session().http_session_vars = Some(r.clone());
            h.bind_session_global(Some(r));
            ok
        }
        Err(e) => {
            throw(h, e);
            false
        }
    };
    if !ok {
        // php_session_cancel_decode()
        destroy(h);
        track_init(h);
        warn(
            h,
            ErrorLevel::Warning,
            "Failed to decode session object. Session has been destroyed",
        );
    }
    ok
}

/// `php_session_create_id()` with the host's CSPRNG; a failing RNG leaves
/// the exception pending.
pub fn create_default_id(h: &mut dyn SessionHost) -> Option<Vec<u8>> {
    let (len, bits) = {
        let c = &h.session().config;
        (c.sid_length, c.sid_bits_per_character)
    };
    match sid::create_id(len, bits, |buf| h.random_bytes(buf)) {
        Ok(id) => Some(id),
        Err(e) => {
            throw(h, e);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Globals reset
// ---------------------------------------------------------------------------

/// `php_rinit_session_globals()`.
pub(crate) fn rinit_globals(h: &mut dyn SessionHost) {
    let s = h.session();
    s.id = None;
    s.status = SessionStatus::None;
    s.in_save_handler = false;
    s.set_handler = false;
    s.mod_data = None;
    s.mod_user_is_open = false;
    s.mod_user_uses_object_methods_as_handlers = false;
    s.define_sid = true;
    s.session_vars = None;
    s.http_session_vars = None;
    s.in_rfc1867_update = false;
}

/// Whether module data is open (`PS(mod_data) || PS(mod_user_implemented)`).
fn module_open(h: &mut dyn SessionHost) -> bool {
    let s = h.session();
    s.mod_data.is_some() || s.mod_user_implemented
}

fn close_current(h: &mut dyn SessionHost) {
    if let Some(m) = h.session().module.clone() {
        mod_close(h, &m);
    }
}

/// `php_rshutdown_session_globals()`.
pub(crate) fn rshutdown_globals(h: &mut dyn SessionHost) {
    h.session().http_session_vars = None;
    if module_open(h) {
        close_current(h);
    }
    let s = h.session();
    s.id = None;
    s.session_vars = None;
    s.started_at = None;
    s.status = SessionStatus::None;
}

/// `php_session_destroy()`.
pub(crate) fn destroy(h: &mut dyn SessionHost) -> bool {
    if h.session().status != SessionStatus::Active {
        warn(
            h,
            ErrorLevel::Warning,
            "Trying to destroy uninitialized session",
        );
        return false;
    }
    let mut ok = true;
    let id = h.session().id.clone();
    let m = h.session().module.clone();
    if let (Some(id), Some(m)) = (id, m) {
        if !mod_destroy(h, &m, id.as_bytes()) {
            ok = false;
            if !h.session().has_exception() {
                warn(h, ErrorLevel::Warning, "Session object destruction failed");
            }
        }
    }
    rshutdown_globals(h);
    rinit_globals(h);
    ok
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// `php_session_gc()`: the number of deleted sessions or `-1`.
pub(crate) fn gc(h: &mut dyn SessionHost, immediate: bool) -> i64 {
    let mut deleted = -1;
    if !module_open(h) {
        return deleted;
    }
    let (probability, divisor, maxlifetime) = {
        let c = &h.session().config;
        (c.gc_probability, c.gc_divisor, c.gc_maxlifetime)
    };
    let mut collect = immediate;
    if !collect && probability > 0 {
        collect = h.gc_random(divisor.saturating_sub(1).max(0)) < probability;
    }
    if collect {
        if let Some(m) = h.session().module.clone() {
            deleted = mod_gc(h, &m, maxlifetime);
        }
    }
    deleted
}

/// Creates a new ID for `php_session_initialize()`.
fn initialize_new_id(h: &mut dyn SessionHost, m: &ModRef) -> bool {
    h.session().id = None;
    if let Some(id) = mod_create_sid(h, m) {
        let s = h.session();
        s.id = Some(ZStr::from(id));
        if s.config.use_cookies {
            s.send_cookie = true;
        }
        true
    } else {
        abort(h);
        if !h.session().has_exception() {
            let (name, path) = mod_name_and_path(h);
            throw(
                h,
                RuntimeError::Error(format!(
                    "Failed to create session ID: {name} (path: {path})"
                )),
            );
        }
        false
    }
}

/// `php_session_initialize()`.
pub(crate) fn initialize(h: &mut dyn SessionHost) -> bool {
    h.session().status = SessionStatus::Active;
    let Some(m) = h.session().module.clone() else {
        h.session().status = SessionStatus::Disabled;
        warn(
            h,
            ErrorLevel::Warning,
            "No storage module chosen - failed to initialize session",
        );
        return false;
    };
    let (save_path, name) = {
        let c = &h.session().config;
        (c.save_path.clone(), c.session_name.clone())
    };
    if !mod_open(h, &m, save_path.as_bytes(), name.as_bytes()) {
        abort(h);
        if !h.session().has_exception() {
            warn(
                h,
                ErrorLevel::Warning,
                &format!(
                    "Failed to initialize storage module: {} (path: {})",
                    m.name(),
                    save_path.to_string_lossy()
                ),
            );
        }
        return false;
    }
    let id = h.session().id.clone();
    match id {
        Some(id) if id.as_bytes().first().is_some_and(|&c| c != 0) => {
            if h.session().config.use_strict_mode
                && !mod_validate_sid(h, &m, id.as_bytes())
                && !initialize_new_id(h, &m)
            {
                return false;
            }
        }
        _ => {
            if !initialize_new_id(h, &m) {
                return false;
            }
        }
    }
    if !reset_id(h) {
        abort(h);
        return false;
    }
    track_init(h);
    let id = h.session().id.clone().unwrap_or_default();
    let maxlifetime = h.session().config.gc_maxlifetime;
    let Some(val) = mod_read(h, &m, id.as_bytes(), maxlifetime) else {
        abort(h);
        if !h.session().has_exception() {
            warn(
                h,
                ErrorLevel::Warning,
                &format!(
                    "Failed to read session data: {} (path: {})",
                    m.name(),
                    save_path.to_string_lossy()
                ),
            );
        }
        return false;
    };
    gc(h, false);
    let lazy = h.session().config.lazy_write;
    h.session().session_vars = if lazy { Some(val.clone()) } else { None };
    decode(h, &val);
    let loc = h.executing_location();
    h.session().started_at = loc;
    true
}

/// `php_session_save_current_state()`.
pub(crate) fn save_current_state(h: &mut dyn SessionHost, write: bool) {
    if write && session_array(h).is_some() {
        let mut ret = false;
        let mut handler = Slot::Write;
        if module_open(h) {
            let Some(val) = encode(h) else {
                close_current(h);
                return;
            };
            let Some(m) = h.session().module.clone() else {
                return;
            };
            let (id, maxlifetime, lazy) = {
                let s = h.session();
                (
                    s.id.clone().unwrap_or_default(),
                    s.config.gc_maxlifetime,
                    s.config.lazy_write,
                )
            };
            let unchanged = lazy && h.session().session_vars.as_deref() == Some(val.as_slice());
            if unchanged && mod_has_update_timestamp(h, &m) {
                ret = mod_update_timestamp(h, &m, id.as_bytes(), &val, maxlifetime);
                if h.session().user.update_timestamp.is_some() {
                    handler = Slot::UpdateTimestamp;
                }
            } else {
                ret = mod_write(h, &m, id.as_bytes(), &val, maxlifetime);
            }
        }
        if !ret && !h.session().has_exception() {
            let (name, path) = mod_name_and_path(h);
            if h.session().mod_user_implemented {
                let callable = h.session().user.get(handler).cloned().unwrap_or_default();
                let cname = h.callable_name(&callable);
                warn(
                    h,
                    ErrorLevel::Warning,
                    &format!(
                        "Failed to write session data using user defined save handler. \
                         (session.save_path: {path}, handler: {cname})"
                    ),
                );
            } else {
                warn(
                    h,
                    ErrorLevel::Warning,
                    &format!(
                        "Failed to write session data ({name}). Please verify that the current \
                         setting of session.save_path is correct ({path})"
                    ),
                );
            }
        }
    }
    if module_open(h) {
        close_current(h);
    }
}

/// `php_session_send_cookie()`.
pub(crate) fn send_cookie(h: &mut dyn SessionHost) -> bool {
    if h.headers_sent() {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session cookie cannot be sent after headers have already been sent",
        );
        return false;
    }
    let (partitioned, secure) = {
        let c = &h.session().config;
        (c.cookie_partitioned, c.cookie_secure)
    };
    if partitioned && !secure {
        warn(
            h,
            ErrorLevel::Warning,
            "Partitioned session cookie cannot be used without also configuring it as secure",
        );
        return false;
    }
    let now = h.now();
    let s = h.session();
    let c = &s.config;
    let id = s.id.clone().unwrap_or_default();
    let line = session_cookie(
        &CookieParams {
            name: c.session_name.as_bytes(),
            id: id.as_bytes(),
            lifetime: c.cookie_lifetime,
            path: c.cookie_path.as_bytes(),
            domain: c.cookie_domain.as_bytes(),
            secure: c.cookie_secure,
            partitioned: c.cookie_partitioned,
            httponly: c.cookie_httponly,
            samesite: c.cookie_samesite.as_bytes(),
        },
        now,
    );
    let mut prefix = b"Set-Cookie: ".to_vec();
    prefix.extend_from_slice(c.session_name.as_bytes());
    prefix.push(b'=');
    h.delete_header_prefix(&prefix);
    h.add_header(&line, false);
    true
}

/// `php_session_reset_id()`.
pub(crate) fn reset_id(h: &mut dyn SessionHost) -> bool {
    let Some(id) = h.session().id.clone() else {
        warn(
            h,
            ErrorLevel::Warning,
            "Cannot set session ID - session ID is not initialized",
        );
        return false;
    };
    let (use_cookies, send) = {
        let s = h.session();
        (s.config.use_cookies, s.send_cookie)
    };
    if use_cookies && send {
        if !send_cookie(h) {
            return false;
        }
        h.session().send_cookie = false;
    }
    let name = h.session().config.session_name.clone();
    if h.session().define_sid {
        let mut v = name.as_bytes().to_vec();
        v.push(b'=');
        v.extend_from_slice(id.as_bytes());
        h.define_sid(&v);
    } else {
        h.define_sid(b"");
    }
    let (trans, only_cookies) = {
        let c = &h.session().config;
        (c.use_trans_sid, c.use_only_cookies)
    };
    if trans && !only_cookies {
        let has_cookie = use_cookies
            && h.request_global("_COOKIE")
                .is_some_and(|c| c.get_str(name.as_bytes()).is_some());
        if !has_cookie {
            h.url_rewrite_session_var(name.as_bytes(), id.as_bytes());
        }
    }
    true
}

/// The mtime of the running script (`last_modified()`).
fn script_mtime(h: &mut dyn SessionHost) -> Option<i64> {
    let path = h.script_path()?;
    let meta = std::fs::metadata(path).ok()?;
    let t = meta.modified().ok()?;
    Some(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    })
}

/// `php_session_cache_limiter()`: 0 = ok/none, -1 = unknown or inactive,
/// -2 = headers already sent (the session is aborted).
pub(crate) fn cache_limiter(h: &mut dyn SessionHost) -> i32 {
    let limiter = h.session().config.cache_limiter.clone();
    if limiter.is_empty() {
        return 0;
    }
    if h.session().status != SessionStatus::Active {
        return -1;
    }
    if h.headers_sent() {
        abort(h);
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session cache limiter cannot be sent after headers have already been sent",
        );
        return -2;
    }
    let Some(l) = CacheLimiter::find(limiter.as_bytes()) else {
        return -1;
    };
    let expire = h.session().config.cache_expire;
    let now = h.now();
    let mtime = if l == CacheLimiter::Nocache {
        None
    } else {
        script_mtime(h)
    };
    for line in l.headers(expire, now, mtime) {
        h.add_header(line.as_bytes(), true);
    }
    0
}

fn proposed_id(h: &mut dyn SessionHost, v: &Value) {
    let s = h.session();
    if let Value::Str(id) = v.deref() {
        s.id = Some(id);
        s.send_cookie = false;
    } else {
        s.id = None;
        s.send_cookie = true;
    }
}

/// `try_find_session_id_in_global()`.
fn find_id_in_global(h: &mut dyn SessionHost, global: &str) {
    if h.session().id.is_some() {
        return;
    }
    let name = h.session().config.session_name.clone();
    if let Some(v) = h
        .request_global(global)
        .and_then(|a| a.get_str(name.as_bytes()).cloned())
    {
        proposed_id(h, &v);
    }
}

/// `try_find_session_id_in_cookies()`.
fn find_id_in_cookies(h: &mut dyn SessionHost) {
    if !h.session().config.use_cookies || h.session().id.is_some() {
        return;
    }
    let name = h.session().config.session_name.clone();
    if let Some(v) = h
        .request_global("_COOKIE")
        .and_then(|a| a.get_str(name.as_bytes()).cloned())
    {
        proposed_id(h, &v);
        let s = h.session();
        s.send_cookie = false;
        s.define_sid = false;
    }
}

/// `should_invalidate_session_for_external_referer()`.
fn external_referer(h: &mut dyn SessionHost) -> bool {
    let chk = h.session().config.referer_check.clone();
    if h.session().id.is_none() || chk.is_empty() {
        return false;
    }
    match h.server_var("HTTP_REFERER").map(|v| v.deref()) {
        Some(Value::Str(r)) if !r.is_empty() => {
            !r.as_bytes().windows(chk.len()).any(|w| w == chk.as_bytes())
        }
        _ => false,
    }
}

/// `php_session_start()`.
pub(crate) fn start(h: &mut dyn SessionHost) -> bool {
    match h.session().status {
        SessionStatus::Active => {
            already_started_error(
                h,
                ErrorLevel::Notice,
                "Ignoring session_start() because a session has already been started",
            );
            return false;
        }
        SessionStatus::Disabled => {
            if h.session().module.is_none() {
                let v = h
                    .session()
                    .ini
                    .get("session.save_handler")
                    .unwrap_or_default()
                    .to_vec();
                let found = h.session().find_module(&v);
                if found.is_none() {
                    warn(
                        h,
                        ErrorLevel::Warning,
                        &format!(
                            "Cannot find session save handler \"{}\" - session startup failed",
                            lossy(&v)
                        ),
                    );
                    return false;
                }
                h.session().module = found;
            }
            if h.session().serializer.is_none() {
                let v = h
                    .session()
                    .ini
                    .get("session.serialize_handler")
                    .unwrap_or_default()
                    .to_vec();
                let found = Serializer::find(&v);
                if found.is_none() {
                    warn(
                        h,
                        ErrorLevel::Warning,
                        &format!(
                            "Cannot find session serialization handler \"{}\" - session startup failed",
                            lossy(&v)
                        ),
                    );
                    return false;
                }
                h.session().serializer = found;
            }
            h.session().status = SessionStatus::None;
        }
        SessionStatus::None => {}
    }
    {
        let s = h.session();
        s.define_sid = !s.config.use_only_cookies;
        s.send_cookie = s.config.use_cookies || s.config.use_only_cookies;
    }
    if h.session().id.is_none() {
        find_id_in_cookies(h);
        if !h.session().config.use_only_cookies {
            find_id_in_global(h, "_GET");
            find_id_in_global(h, "_POST");
            if external_referer(h) {
                h.session().id = None;
            }
        }
    }
    if h.session()
        .id
        .as_ref()
        .is_some_and(|id| sid::has_dangerous_chars(id.as_bytes()))
    {
        h.session().id = None;
    }
    if !initialize(h) || cache_limiter(h) == -2 {
        let s = h.session();
        s.status = SessionStatus::None;
        s.id = None;
        return false;
    }
    true
}

/// `php_session_flush()`.
pub(crate) fn flush(h: &mut dyn SessionHost, write: bool) -> bool {
    if h.session().status == SessionStatus::Active {
        save_current_state(h, write);
        h.session().status = SessionStatus::None;
        return true;
    }
    false
}

/// `php_session_abort()`.
pub(crate) fn abort(h: &mut dyn SessionHost) -> bool {
    if h.session().status == SessionStatus::Active {
        if module_open(h) {
            close_current(h);
        }
        h.session().status = SessionStatus::None;
        return true;
    }
    false
}

/// `php_session_reset()`.
pub(crate) fn reset(h: &mut dyn SessionHost) -> bool {
    if h.session().status != SessionStatus::Active {
        return false;
    }
    abort(h);
    initialize(h)
}

/// `RINIT`: resets the request globals, resolves the module and the
/// serializer from the INI values and honours `session.auto_start`.
pub fn request_startup(h: &mut dyn SessionHost) -> RtResult<()> {
    h.session().modules_activated = false;
    let auto_start = h.session().config.auto_start;
    rinit_session(h, auto_start);
    h.session().modules_activated = true;
    finish(h, ())
}

/// `php_rinit_session(auto_start)`.
pub(crate) fn rinit_session(h: &mut dyn SessionHost, auto_start: bool) {
    rinit_globals(h);
    let handler = h
        .session()
        .ini
        .get("session.save_handler")
        .unwrap_or_default()
        .to_vec();
    let module = h.session().find_module(&handler);
    h.session().module = module;
    if h.session().serializer.is_none() {
        let v = h
            .session()
            .ini
            .get("session.serialize_handler")
            .unwrap_or_default()
            .to_vec();
        h.session().serializer = Serializer::find(&v);
    }
    let usable = h.session().module.is_some() && h.session().serializer.is_some();
    if !usable {
        h.session().status = SessionStatus::Disabled;
    } else if auto_start {
        start(h);
    }
}

/// `RSHUTDOWN`: writes and closes an active session and frees the user
/// handlers. Call [`crate::ini::deactivate`] afterwards.
pub fn request_shutdown(h: &mut dyn SessionHost) -> RtResult<()> {
    if h.session().status == SessionStatus::Active {
        flush(h, true);
    }
    rshutdown_globals(h);
    h.session().user = UserHandlers::default();
    finish(h, ())
}

/// `MINIT`: applies configuration values (`php.ini`, `-d`) to the session
/// directives. `config` looks a directive up by name.
pub fn module_startup(h: &mut dyn SessionHost, config: &dyn Fn(&str) -> Option<Vec<u8>>) {
    h.session().status = SessionStatus::None;
    crate::ini::register(h, config);
}

/// Validates an ID with the module (`s_validate_sid`) — used by
/// `session_regenerate_id()`/`session_create_id()`.
pub(crate) fn validate(h: &mut dyn SessionHost, m: &ModRef, id: &[u8]) -> bool {
    mod_validate_sid(h, m, id)
}

/// Writes `data` with the current module (`s_write`).
pub(crate) fn write_with(h: &mut dyn SessionHost, m: &ModRef, id: &[u8], data: &[u8]) -> bool {
    let maxlifetime = h.session().config.gc_maxlifetime;
    mod_write(h, m, id, data, maxlifetime)
}

/// Reads with the current module (`s_read`).
pub(crate) fn read_with(h: &mut dyn SessionHost, m: &ModRef, id: &[u8]) -> Option<Vec<u8>> {
    let maxlifetime = h.session().config.gc_maxlifetime;
    mod_read(h, m, id, maxlifetime)
}

/// Opens the current module with the configured path and name.
pub(crate) fn open_with(h: &mut dyn SessionHost, m: &ModRef) -> bool {
    let (save_path, name) = {
        let c = &h.session().config;
        (c.save_path.clone(), c.session_name.clone())
    };
    mod_open(h, m, save_path.as_bytes(), name.as_bytes())
}

/// Messages need the module name and save path.
pub(crate) fn name_and_path(h: &mut dyn SessionHost) -> (String, String) {
    mod_name_and_path(h)
}

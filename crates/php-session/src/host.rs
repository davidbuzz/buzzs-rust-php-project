//! The interface between the session module and its embedder (the VM/SAPI).

use crate::state::SessionState;
use php_runtime::serialize::UnserializeOptions;
use php_runtime::{ErrorLevel, Obj, PhpRef, RtResult, RuntimeContext, RuntimeError, Value, ZArray};
use std::path::PathBuf;

/// What the session module needs from the VM and the SAPI.
///
/// The per-request [`SessionState`] lives *inside* the host and is reached
/// through [`SessionHost::session`]. Every session entry point is a free
/// function taking `&mut dyn SessionHost` and never holds a borrow of the
/// state across [`SessionHost::call_handler`]: user save handlers routinely
/// re-enter the module (`parent::open()` on `SessionHandler`,
/// `session_id()`, `session_status()`…), exactly like PHP's `PS()` globals.
///
/// Diagnostics: [`SessionHost::emit_diagnostic`] has `php_error_docref`
/// semantics (the host prefixes the active function, `"PHP Startup"`,
/// `"PHP Request Shutdown"`…); the inherited
/// [`RuntimeContext::diagnostic`] is used for plain `zend_error` messages.
pub trait SessionHost: RuntimeContext {
    /// The per-request session state (`PS()` globals).
    fn session(&mut self) -> &mut SessionState;

    /// The `var_hash` options session data is unserialized with
    /// (`unserialize_max_depth`, and diagnostics naming the active function
    /// like `php_error_docref`). The default uses php-runtime's defaults.
    fn unserialize_options(&mut self) -> UnserializeOptions {
        UnserializeOptions::default()
    }

    /// `php_var_unserialize()` of the one value at `offset` in `data` within
    /// the active `unserialize()` call's shared `var_hash` (so a
    /// `Serializable::unserialize()` calling `session_decode()` resolves
    /// `r:`/`R:` against the outer call's slots): `Ok(Ok((value, end)))`,
    /// `Ok(Err(failure_offset))`, or `Err` for an exception. `None` (the
    /// default, and whenever no `unserialize()` call is active) makes the
    /// module parse the data itself with [`SessionHost::unserialize_options`].
    fn unserialize_shared(
        &mut self,
        data: &[u8],
        offset: usize,
    ) -> Option<RtResult<Result<(Value, usize), usize>>> {
        let _ = (data, offset);
        None
    }

    /// `php_error_docref(NULL, level, "%s", message)`. Returning `Err`
    /// models a user error handler throwing; the session module keeps going
    /// and reports the exception when the current entry point returns.
    fn emit_diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()>;

    /// `call_user_function()` on a user save-handler callable (a string, an
    /// `[object, "method"]` array or a closure). `Ok(None)` means the call
    /// failed without an exception (the retval is `UNDEF`); `Err` is a
    /// thrown exception. When the exception cannot propagate (no user frame,
    /// e.g. during request shutdown) PHP turns it into a fatal error and
    /// bails out: the host prints it and returns [`RuntimeError::Fatal`]
    /// (also used for `exit`), after which the module behaves as after a
    /// `longjmp` (the next handler call reports the recursion).
    fn call_handler(&mut self, callable: &Value, args: &[Value]) -> RtResult<Option<Value>>;

    /// `zend_get_callable_name()` (closures are named after their function,
    /// e.g. `{closure:/path/file.php:12}`). The default handles strings,
    /// `[object|class, method]` arrays and invokable objects.
    fn callable_name(&mut self, callable: &Value) -> String {
        crate::user::callable_name(callable)
    }

    /// `zend_is_callable_ex()` for the `f` ZPP specifier:
    /// `Err("function \"echo\" not found or invalid function name")`.
    fn check_callable(&mut self, callable: &Value) -> Result<(), String>;

    /// `instanceof_function(Z_OBJCE_P(obj), class)` (`class` is one of the
    /// session interfaces, e.g. `"SessionIdInterface"`).
    fn instance_of(&mut self, obj: &Obj, class: &str) -> bool;

    /// Whether the object's class has a method named `method_lc`
    /// (lower-case lookup in `function_table`).
    fn has_method(&mut self, obj: &Obj, method_lc: &str) -> bool;

    /// Installs `$_SESSION` in the global symbol table as a reference to
    /// `slot` (replacing any existing variable), or deletes it for `None`.
    fn bind_session_global(&mut self, slot: Option<PhpRef>);

    /// `SG(headers_sent)`.
    fn headers_sent(&self) -> bool;

    /// Where output started (`php_output_get_start_filename/lineno`).
    fn output_start(&self) -> Option<(String, u32)> {
        None
    }

    /// `sapi_add_header_ex(line, replace)`; `line` is `"Name: value"`.
    fn add_header(&mut self, line: &[u8], replace: bool);

    /// `sapi_header_op(SAPI_HEADER_DELETE_PREFIX)`: removes every header
    /// longer than `prefix` that starts with it (case-insensitively); used
    /// with `"Set-Cookie: <name>="`.
    fn delete_header_prefix(&mut self, prefix: &[u8]);

    /// The current value of a superglobal in `EG(symbol_table)` (`"_COOKIE"`,
    /// `"_GET"`, `"_POST"`), dereferenced, when it is an array.
    fn request_global(&mut self, name: &str) -> Option<ZArray>;

    /// `PG(http_globals)[TRACK_VARS_SERVER][name]` (e.g. `HTTP_REFERER`).
    fn server_var(&mut self, name: &str) -> Option<Value> {
        let _ = name;
        None
    }

    /// `SG(request_info).path_translated` (the script) for `Last-Modified`.
    fn script_path(&self) -> Option<PathBuf> {
        None
    }

    /// The current UNIX time.
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }

    /// The currently executing file and line (`zend_get_executed_filename_ex`
    /// / `zend_get_executed_lineno`), `None` outside user code.
    fn executing_location(&self) -> Option<(String, u32)> {
        None
    }

    /// `php_get_temporary_directory()`.
    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }

    /// `php_check_open_basedir(path)`: `true` when access is *denied* (the
    /// host emits the "`open_basedir` restriction in effect" warning).
    fn open_basedir_denied(&mut self, path: &[u8]) -> bool {
        let _ = path;
        false
    }

    /// `php_random_bytes_throw()`; failing throws `Random\RandomException`.
    fn random_bytes(&mut self, buf: &mut [u8]) -> RtResult<()> {
        php_random::csprng::fill(buf).map_err(|m| RuntimeError::Custom {
            class: "Random\\RandomException".into(),
            message: m.to_string(),
        })
    }

    /// A uniform integer in `[0, max]` for the GC probability draw (PHP
    /// uses a lazily CSPRNG-seeded PCG; only the distribution matters).
    fn gc_random(&mut self, max: i64) -> i64 {
        php_random::csprng::random_int_unchecked(0, max).unwrap_or(0)
    }

    /// Defines (or redefines) the deprecated `SID` constant.
    fn define_sid(&mut self, value: &[u8]) {
        let _ = value;
    }

    /// `php_url_scanner_reset_session_var(name, true)` followed by
    /// `php_url_scanner_add_session_var(name, id, true)` (trans-sid).
    fn url_rewrite_session_var(&mut self, name: &[u8], id: &[u8]) {
        let _ = (name, id);
    }

    /// `register_user_shutdown_function("session_shutdown", …)` calling
    /// `session_register_shutdown()` (replacing an existing one).
    fn register_session_shutdown(&mut self) -> bool {
        true
    }

    /// `zend_alter_ini_entry_ex(name, value, PHP_INI_USER, RUNTIME)` for a
    /// `session.*` directive owned by another extension (e.g.
    /// `session.trans_sid_tags` of the URL rewriter), used by
    /// `session_start()` options. `false` = unknown or rejected.
    fn alter_foreign_ini(&mut self, name: &str, value: &[u8]) -> bool {
        let _ = (name, value);
        false
    }

    /// `remove_user_shutdown_function("session_shutdown")`.
    fn remove_session_shutdown(&mut self) {}

    /// `append_user_shutdown_function()` calling `session_write_close()`.
    fn append_write_close_shutdown(&mut self) -> bool {
        true
    }
}

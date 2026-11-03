//! The interface to the embedding VM ([`Host`]) and the per-call context
//! ([`Cx`]) that tracks the pending exception like `EG(exception)`.

use crate::error::{Exception, ExceptionClass, PhpError};
use php_runtime::{ErrorLevel, RuntimeContext, RuntimeError, Value};

/// What this crate needs from the PHP VM.
///
/// The supertrait [`RuntimeContext`] provides plain diagnostics (`zend_error`
/// style, used by value conversions such as "Array to string conversion")
/// and object hooks for the conversions themselves.
pub trait Host: RuntimeContext {
    /// Calls a PHP callable with positional arguments
    /// (`zend_call_known_fcc`). Arguments may be [`Value::Reference`]s: the
    /// VM passes them by reference to by-reference parameters and
    /// dereferences them otherwise. `Err` is an exception thrown by the
    /// callee.
    fn call(&mut self, callable: &Value, args: &[Value]) -> Result<Value, RuntimeError>;

    /// `php_error_docref(NULL, level, msg)`: the VM prefixes the active
    /// function name (`"SQLite3::exec(): "`). Defaults to
    /// [`RuntimeContext::diagnostic`].
    fn docref(&mut self, level: ErrorLevel, message: &str) -> Result<(), RuntimeError> {
        self.diagnostic(level, message)
    }

    /// `get_active_function_or_method_name()` (e.g. `"PDO::query"`), used by
    /// messages that embed it.
    fn active_function_name(&self) -> String {
        String::new()
    }

    /// `php_stream_from_zval_no_verify()` + `php_stream_copy_to_mem()`:
    /// the remaining contents of a stream resource, or `None` when the value
    /// is not a stream.
    fn read_stream(&mut self, value: &Value) -> Option<Vec<u8>> {
        let _ = value;
        None
    }

    /// `expand_filepath()`: makes a path absolute against the (virtual)
    /// working directory; `None` on failure.
    fn expand_filepath(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        default_expand_filepath(path)
    }

    /// Whether an `open_basedir` restriction is configured
    /// (`PG(open_basedir) && *PG(open_basedir)`).
    fn open_basedir_active(&self) -> bool {
        false
    }

    /// `php_check_open_basedir(path)`: `true` when access is *denied* (the
    /// VM emits the "`open_basedir` restriction in effect" warning itself).
    fn check_open_basedir(&mut self, path: &[u8]) -> bool {
        let _ = path;
        false
    }

    /// `VCWD_REALPATH()`: the canonical path, or `None` when it does not
    /// exist.
    fn realpath(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        default_realpath(path)
    }

    /// The `sqlite3.extension_dir` INI setting (`None` when unset).
    fn sqlite3_extension_dir(&self) -> Option<Vec<u8>> {
        None
    }

    /// The `sqlite3.defensive` INI setting (default `1`).
    fn sqlite3_defensive(&self) -> bool {
        true
    }
}

fn default_expand_filepath(path: &[u8]) -> Option<Vec<u8>> {
    if path.first() == Some(&b'/') {
        return Some(path.to_vec());
    }
    let cwd = std::env::current_dir().ok()?;
    let mut out = cwd.into_os_string().into_encoded_bytes();
    if !out.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(path);
    Some(out)
}

fn default_realpath(path: &[u8]) -> Option<Vec<u8>> {
    let p = std::path::PathBuf::from(std::ffi::OsString::from(
        String::from_utf8_lossy(path).into_owned(),
    ));
    let real = std::fs::canonicalize(p).ok()?;
    Some(real.into_os_string().into_encoded_bytes())
}

/// The context of one API call: the host plus the pending exception
/// (`EG(exception)`), which callbacks set and later code inspects.
pub struct Cx<'h> {
    host: &'h mut dyn Host,
    pending: Option<PhpError>,
}

impl std::fmt::Debug for Cx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cx")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl<'h> Cx<'h> {
    /// Starts a call context.
    pub fn new(host: &'h mut dyn Host) -> Self {
        Cx {
            host,
            pending: None,
        }
    }

    /// The host.
    pub fn host(&mut self) -> &mut dyn Host {
        &mut *self.host
    }

    /// Whether an exception is pending (`EG(exception)`).
    pub fn has_exception(&self) -> bool {
        self.pending.is_some()
    }

    /// Calls a user callable like `zend_call_known_fcc()`: nothing happens
    /// (and `None`, i.e. an `UNDEF` return value, results) while an
    /// exception is pending; an exception thrown by the callee becomes
    /// pending.
    pub fn call(&mut self, callable: &Value, args: &[Value]) -> Option<Value> {
        if self.pending.is_some() {
            return None;
        }
        match self.host.call(callable, args) {
            Ok(v) => Some(v),
            Err(e) => {
                self.pending = Some(PhpError::Runtime(e));
                None
            }
        }
    }

    /// Throws an exception (`zend_throw_exception`): a pending exception
    /// becomes its previous one.
    pub fn throw(&mut self, mut e: PhpError) {
        if let Some(prev) = self.pending.take() {
            e.set_innermost_previous(prev);
        }
        self.pending = Some(e);
    }

    /// Throws an extension exception.
    pub fn throw_new(&mut self, class: ExceptionClass, message: impl Into<String>, code: i64) {
        self.throw(PhpError::Exception(Exception::new(class, message, code)));
    }

    /// Records the result of a runtime operation (conversions): an error
    /// becomes the pending exception.
    pub fn check<T>(&mut self, r: Result<T, RuntimeError>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.throw(PhpError::Runtime(e));
                None
            }
        }
    }

    /// `php_error_docref(NULL, level, "%s", msg)`.
    pub fn docref(&mut self, level: ErrorLevel, message: &str) {
        let r = self.host.docref(level, message);
        self.check(r);
    }

    /// `php_error_docref(NULL, E_WARNING, ...)`.
    pub fn warning(&mut self, message: &str) {
        self.docref(ErrorLevel::Warning, message);
    }

    /// `zend_error(level, msg)` (no function prefix).
    pub fn error(&mut self, level: ErrorLevel, message: &str) {
        let r = self.host.diagnostic(level, message);
        self.check(r);
    }

    /// Takes the pending exception.
    pub fn take_exception(&mut self) -> Option<PhpError> {
        self.pending.take()
    }

    /// Finishes the call: the pending exception wins over the return value.
    pub fn finish<T>(mut self, value: T) -> Result<T, PhpError> {
        match self.pending.take() {
            Some(e) => Err(e),
            None => Ok(value),
        }
    }
}

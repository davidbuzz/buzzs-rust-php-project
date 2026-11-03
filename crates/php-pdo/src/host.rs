//! The interface to the embedding VM ([`PdoHost`]) and the per-call context
//! ([`Ctx`]) that tracks the pending exception like `EG(exception)`.

use crate::dbh::Pdo;
use crate::persistent::PersistentList;
use crate::row::PdoRow;
use crate::stmt::PdoStatement;
use php_runtime::{
    BinaryOp, CastTarget, ErrorLevel, Obj, ObjectDump, PropertyPurpose, RtResult, RuntimeContext,
    RuntimeError, Value, ZArray, ZStr,
};

/// What the PDO core needs to know about a class (`zend_class_entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassInfo {
    /// The declared (canonical) class name.
    pub name: ZStr,
    /// Whether the class has a constructor (`ce->constructor != NULL`).
    pub has_constructor: bool,
    /// Whether that constructor is public (neither private nor protected).
    pub constructor_is_public: bool,
    /// Whether the class can be built by `ce->unserialize` (it implements
    /// `Serializable`), used by the deprecated `PDO::FETCH_SERIALIZE`.
    pub can_unserialize: bool,
}

impl ClassInfo {
    /// A class without a constructor.
    pub fn plain(name: impl Into<ZStr>) -> Self {
        ClassInfo {
            name: name.into(),
            has_constructor: false,
            constructor_is_public: false,
            can_unserialize: false,
        }
    }
}

/// The data of a `PDOException` the core throws: the VM instantiates the
/// class and sets the `message`, `code` and `errorInfo` properties.
#[derive(Debug, Clone)]
pub struct PdoException {
    /// `$message`.
    pub message: String,
    /// `$code`: a SQLSTATE string (`pdo_handle_error`) or a native error
    /// number (`pdo_throw_exception`, plain `zend_throw_exception`).
    pub code: Value,
    /// `$errorInfo` (left `null` when `None`).
    pub error_info: Option<ZArray>,
}

impl PdoException {
    /// A `PDOException` with an integer code and no `errorInfo`
    /// (`zend_throw_exception(pdo_exception_ce, msg, code)`).
    pub fn new(message: impl Into<String>, code: i64) -> Self {
        PdoException {
            message: message.into(),
            code: Value::Int(code),
            error_info: None,
        }
    }
}

/// What the PDO core needs from the PHP VM.
///
/// The supertrait [`RuntimeContext`] provides diagnostics and the object
/// hooks that value conversions use. Methods with a default body are
/// optional.
pub trait PdoHost: RuntimeContext {
    // ----- calls ---------------------------------------------------------

    /// Calls a PHP callable with positional arguments
    /// (`zend_call_known_fcc`); `Err` is an exception thrown by the callee.
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value>;

    /// `zend_is_callable_ex()`: `Err` carries the error text (for example
    /// `"function \"nope\" not found or invalid function name"`).
    fn check_callable(&mut self, callable: &Value) -> Result<(), String>;

    /// `php_error_docref(NULL, level, msg)`: the VM prefixes the active
    /// function name (`"PDOStatement::execute(): "`).
    fn docref(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.diagnostic(level, message)
    }

    /// `get_active_function_or_method_name()` (e.g. `"PDO::__construct"`),
    /// used as the prefix of warnings turned into exceptions while the
    /// constructor runs with `EH_THROW` error handling.
    fn active_function_name(&self) -> String {
        String::new()
    }

    // ----- classes and objects -------------------------------------------

    /// `zend_lookup_class()` (with autoloading): `None` when the class does
    /// not exist.
    fn lookup_class(&mut self, name: &ZStr) -> Option<ClassInfo>;

    /// `instanceof_function(class, parent)`: whether `class` is `parent` or
    /// extends / implements it (case-insensitive class names).
    fn instance_of(&mut self, class: &ZStr, parent: &str) -> bool;

    /// `object_init_ex()`: creates an instance of `class` without calling
    /// its constructor (the VM throws "Cannot instantiate abstract class
    /// ..." and similar errors).
    fn instantiate(&mut self, class: &ZStr) -> RtResult<Value>;

    /// Calls the constructor of `obj`'s class, if it has one, regardless of
    /// its visibility (`zend_call_known_function(ce->constructor, ...)`),
    /// passing `args` as positional and (string keys) named arguments.
    fn call_constructor(&mut self, obj: &Value, args: Option<&ZArray>) -> RtResult<()>;

    /// `zend_update_property_ex(scope, obj, name, value)`: writes a property
    /// as code running in `scope` (`None` = global scope) would, honouring
    /// visibility, `readonly` and `__set`.
    fn update_property(
        &mut self,
        obj: &Value,
        scope: Option<&ZStr>,
        name: &ZStr,
        value: Value,
    ) -> RtResult<()>;

    /// `ce->unserialize(object, ce, data)` for `PDO::FETCH_SERIALIZE`
    /// (`Serializable::unserialize` on a new instance).
    fn unserialize_class(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        let _ = data;
        Err(RuntimeError::Error(format!(
            "Cannot unserialize object of class {class}"
        )))
    }

    // ----- PDO objects -----------------------------------------------------

    /// Creates an object of `class` (`PDOStatement` or a subclass) without
    /// calling its constructor; the VM attaches a fresh [`PdoStatement`]
    /// handle to it (see [`PdoStatement::attach_object`]) and returns both.
    fn instantiate_statement(&mut self, class: &ZStr) -> RtResult<(Value, PdoStatement)>;

    /// Creates an object of `class` (`PDO` or a subclass) without calling
    /// its constructor, for `PDO::connect()`; the VM attaches a fresh
    /// [`Pdo`] handle to it and returns both.
    fn instantiate_pdo(&mut self, class: &ZStr) -> RtResult<(Value, Pdo)>;

    /// Writes a declared property with `zend_std_write_property` semantics
    /// (bypassing the `PDOStatement` read-only `queryString` handler).
    fn std_write_property(&mut self, obj: &Value, name: &ZStr, value: Value) -> RtResult<()>;

    /// Creates the `PDORow` object of a statement (`PDO::FETCH_LAZY`).
    fn create_row(&mut self, row: PdoRow) -> RtResult<Value>;

    /// Builds the `PDOException` object to throw.
    fn new_pdo_exception(&mut self, exception: PdoException) -> RuntimeError {
        RuntimeError::Custom {
            class: ZStr::from("PDOException"),
            message: exception.message,
        }
    }

    /// `zend_exception_set_previous()`: makes `previous` the previous
    /// exception of `exception` (an exception thrown while another one is
    /// pending). The default keeps only the new exception.
    fn chain_exception(&mut self, exception: RuntimeError, previous: RuntimeError) -> RuntimeError {
        let _ = previous;
        exception
    }

    // ----- streams and output --------------------------------------------

    /// `php_stream_from_zval_no_verify()` + `php_stream_copy_to_mem()`: the
    /// remaining contents of a stream resource, `None` when the value is not
    /// a stream.
    fn read_stream(&mut self, value: &Value) -> Option<Vec<u8>> {
        let _ = value;
        None
    }

    /// `php_stream_memory_open(TEMP_STREAM_READONLY, data)` as a resource
    /// (`PDO::PARAM_LOB` columns).
    fn memory_stream(&mut self, data: ZStr) -> RtResult<Value> {
        Ok(Value::Str(data))
    }

    /// `php_stream_open_wrapper(uri, "rb", REPORT_ERRORS)` +
    /// `php_stream_get_line(stream, buf, max)`: the first line of `uri`
    /// (including its end of line, at most `max - 1` bytes), `None` when
    /// the stream cannot be opened or is empty.
    fn read_uri_line(&mut self, uri: &[u8], max: usize) -> Option<Vec<u8>> {
        let _ = (uri, max);
        None
    }

    /// Writes to `php://output` (`debugDumpParams()`).
    fn write_output(&mut self, bytes: &[u8]) -> RtResult<()>;

    // ----- configuration ---------------------------------------------------

    /// `cfg_get_string(name)`: a `php.ini` directive such as
    /// `pdo.dsn.<alias>`.
    fn cfg_get_string(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let _ = name;
        None
    }

    /// The persistent-connection list (`EG(persistent_list)`), which
    /// outlives requests.
    fn persistent_list(&mut self) -> &mut PersistentList;

    // ----- pass-through for the SQLite driver ------------------------------

    /// `expand_filepath()` (see [`php_sqlite::Host::expand_filepath`]).
    fn expand_filepath(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        php_sqlite::Host::expand_filepath(&mut SqliteDefaults, path)
    }

    /// Whether `open_basedir` is configured.
    fn open_basedir_active(&self) -> bool {
        false
    }

    /// `php_check_open_basedir(path)`: `true` when access is denied.
    fn check_open_basedir(&mut self, path: &[u8]) -> bool {
        let _ = path;
        false
    }

    /// `VCWD_REALPATH()`.
    fn realpath(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        php_sqlite::Host::realpath(&mut SqliteDefaults, path)
    }
}

/// Access to `php_sqlite::Host`'s default path helpers.
struct SqliteDefaults;

impl RuntimeContext for SqliteDefaults {
    fn diagnostic(&mut self, _: ErrorLevel, _: &str) -> RtResult<()> {
        Ok(())
    }
}

impl php_sqlite::Host for SqliteDefaults {
    fn call(&mut self, _: &Value, _: &[Value]) -> RtResult<Value> {
        Ok(Value::Null)
    }
}

/// The driver-side call context: drivers receive the `php-sqlite` call
/// context, whose host is the [`PdoHost`] seen through
/// [`php_sqlite::Host`], and leave exceptions pending in it.
pub type DriverCx<'a> = php_sqlite::Cx<'a>;

/// The context of one API call: the host, the pending exception
/// (`EG(exception)`) and the error handling mode.
pub struct Ctx<'h> {
    host: &'h mut dyn PdoHost,
    pending: Option<RuntimeError>,
    /// `zend_replace_error_handling(EH_THROW, pdo_exception_ce)`: warnings
    /// become `PDOException`s.
    throw_warnings: bool,
}

impl std::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("pending", &self.pending)
            .field("throw_warnings", &self.throw_warnings)
            .finish_non_exhaustive()
    }
}

impl<'h> Ctx<'h> {
    /// Starts a call context.
    pub fn new(host: &'h mut dyn PdoHost) -> Self {
        Ctx {
            host,
            pending: None,
            throw_warnings: false,
        }
    }

    /// The host.
    pub fn host(&mut self) -> &mut dyn PdoHost {
        &mut *self.host
    }

    /// Whether an exception is pending.
    pub fn has_exception(&self) -> bool {
        self.pending.is_some()
    }

    /// Throws an exception; a pending one becomes its previous exception.
    pub fn throw(&mut self, e: RuntimeError) {
        let e = match self.pending.take() {
            Some(prev) => self.host.chain_exception(e, prev),
            None => e,
        };
        self.pending = Some(e);
    }

    /// Throws a `PDOException`.
    pub fn throw_pdo(&mut self, e: PdoException) {
        let e = self.host.new_pdo_exception(e);
        self.throw(e);
    }

    /// `zend_throw_error(NULL, msg)`.
    pub fn throw_error(&mut self, msg: impl Into<String>) {
        self.throw(RuntimeError::Error(msg.into()));
    }

    /// `zend_type_error(msg)`.
    pub fn throw_type_error(&mut self, msg: impl Into<String>) {
        self.throw(RuntimeError::TypeError(msg.into()));
    }

    /// `zend_value_error(msg)`.
    pub fn throw_value_error(&mut self, msg: impl Into<String>) {
        self.throw(RuntimeError::ValueError(msg.into()));
    }

    /// Records the result of a runtime operation: an error becomes the
    /// pending exception.
    pub fn check<T>(&mut self, r: RtResult<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.throw(e);
                None
            }
        }
    }

    /// `php_error_docref(NULL, level, "%s", msg)`, honouring `EH_THROW`
    /// for warnings.
    pub fn docref(&mut self, level: ErrorLevel, message: &str) {
        if self.throw_warnings && level == ErrorLevel::Warning {
            if self.pending.is_none() {
                let func = self.host.active_function_name();
                let msg = if func.is_empty() {
                    message.to_string()
                } else {
                    format!("{func}(): {message}")
                };
                self.throw_pdo(PdoException::new(msg, 0));
            }
            return;
        }
        let r = self.host.docref(level, message);
        self.check(r);
    }

    /// `php_error_docref(NULL, E_WARNING, ...)`.
    pub fn warning(&mut self, message: &str) {
        self.docref(ErrorLevel::Warning, message);
    }

    /// `zend_error(level, msg)` (no function prefix).
    pub fn error(&mut self, level: ErrorLevel, message: &str) {
        if self.throw_warnings && level == ErrorLevel::Warning {
            if self.pending.is_none() {
                self.throw_pdo(PdoException::new(message, 0));
            }
            return;
        }
        let r = self.host.diagnostic(level, message);
        self.check(r);
    }

    /// Takes the pending exception.
    pub fn take_exception(&mut self) -> Option<RuntimeError> {
        self.pending.take()
    }

    /// Sets the `EH_THROW` mode, returning the previous one.
    pub(crate) fn set_throw_warnings(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.throw_warnings, on)
    }

    /// Finishes the call: the pending exception wins over the value.
    pub fn finish<T>(mut self, value: T) -> RtResult<T> {
        match self.pending.take() {
            Some(e) => Err(e),
            None => Ok(value),
        }
    }

    /// Runs a driver method with a [`DriverCx`] over this context; an
    /// exception the driver leaves pending becomes pending here.
    pub fn with_driver<R>(&mut self, f: impl FnOnce(&mut DriverCx<'_>) -> R) -> R {
        let throw_warnings = self.throw_warnings;
        let mut adapter = DriverHost {
            host: &mut *self.host,
            throw_warnings,
        };
        let (r, exc) = {
            let mut dcx = php_sqlite::Cx::new(&mut adapter);
            let r = f(&mut dcx);
            (r, dcx.take_exception())
        };
        if let Some(e) = exc {
            let e = convert_php_error(&mut *self.host, e);
            self.throw(e);
        }
        r
    }
}

/// Converts an exception raised by the SQLite crate into a runtime error.
pub(crate) fn convert_php_error(host: &mut dyn PdoHost, e: php_sqlite::PhpError) -> RuntimeError {
    use php_sqlite::{ExceptionClass, PhpError};
    match e {
        PhpError::Runtime(r) => r,
        PhpError::Exception(ex) => {
            let base = match ex.class {
                ExceptionClass::Error => RuntimeError::Error(ex.message),
                ExceptionClass::TypeError => RuntimeError::TypeError(ex.message),
                ExceptionClass::ValueError => RuntimeError::ValueError(ex.message),
                ExceptionClass::PdoException => {
                    host.new_pdo_exception(PdoException::new(ex.message, ex.code))
                }
                ExceptionClass::Exception | ExceptionClass::Sqlite3Exception => {
                    RuntimeError::Custom {
                        class: ZStr::from(ex.class.name()),
                        message: ex.message,
                    }
                }
            };
            match ex.previous {
                Some(prev) => {
                    let prev = convert_php_error(host, *prev);
                    host.chain_exception(base, prev)
                }
                None => base,
            }
        }
    }
}

/// The [`PdoHost`] seen as a [`php_sqlite::Host`].
struct DriverHost<'a> {
    host: &'a mut dyn PdoHost,
    throw_warnings: bool,
}

impl RuntimeContext for DriverHost<'_> {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        if self.throw_warnings && level == ErrorLevel::Warning {
            return Err(self.host.new_pdo_exception(PdoException::new(message, 0)));
        }
        self.host.diagnostic(level, message)
    }
    fn precision(&self) -> i64 {
        self.host.precision()
    }
    fn serialize_precision(&self) -> i64 {
        self.host.serialize_precision()
    }
    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        self.host.cast_object(obj, target)
    }
    fn do_operation(&mut self, op: BinaryOp, op1: &Value, op2: &Value) -> RtResult<Option<Value>> {
        self.host.do_operation(op, op1, op2)
    }
    fn compare_objects(&mut self, op1: &Value, op2: &Value) -> RtResult<i32> {
        self.host.compare_objects(op1, op2)
    }
    fn object_properties(
        &mut self,
        obj: &Obj,
        purpose: PropertyPurpose,
    ) -> RtResult<Option<ZArray>> {
        self.host.object_properties(obj, purpose)
    }
    fn object_dump(&mut self, obj: &Obj, purpose: PropertyPurpose) -> RtResult<ObjectDump> {
        self.host.object_dump(obj, purpose)
    }
    fn serialize_object(
        &mut self,
        obj: &Obj,
    ) -> RtResult<php_runtime::serialize::SerializedObject> {
        self.host.serialize_object(obj)
    }
    fn unserialize_object_create(&mut self, class: &ZStr) -> RtResult<Value> {
        self.host.unserialize_object_create(class)
    }
    fn unserialize_object_finish(&mut self, obj: &Value, props: ZArray) -> RtResult<()> {
        self.host.unserialize_object_finish(obj, props)
    }
    fn unserialize_enum(&mut self, class: &ZStr, case: &ZStr) -> RtResult<Value> {
        self.host.unserialize_enum(class, case)
    }
    fn unserialize_custom(&mut self, class: &ZStr, data: &[u8]) -> RtResult<Value> {
        self.host.unserialize_custom(class, data)
    }
    fn is_enum(&mut self, obj: &Obj) -> bool {
        self.host.is_enum(obj)
    }
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        self.host.count_object(obj)
    }
    fn property_read(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        self.host.property_read(obj, name)
    }
    fn property_write(&mut self, obj: &Obj, name: &ZStr, value: Value) -> RtResult<()> {
        self.host.property_write(obj, name, value)
    }
    fn create_std_object(&mut self, props: ZArray) -> RtResult<Value> {
        self.host.create_std_object(props)
    }
}

impl php_sqlite::Host for DriverHost<'_> {
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        self.host.call(callable, args)
    }
    fn docref(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        if self.throw_warnings && level == ErrorLevel::Warning {
            let func = self.host.active_function_name();
            let msg = if func.is_empty() {
                message.to_string()
            } else {
                format!("{func}(): {message}")
            };
            return Err(self.host.new_pdo_exception(PdoException::new(msg, 0)));
        }
        self.host.docref(level, message)
    }
    fn active_function_name(&self) -> String {
        self.host.active_function_name()
    }
    fn read_stream(&mut self, value: &Value) -> Option<Vec<u8>> {
        self.host.read_stream(value)
    }
    fn expand_filepath(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        self.host.expand_filepath(path)
    }
    fn open_basedir_active(&self) -> bool {
        self.host.open_basedir_active()
    }
    fn check_open_basedir(&mut self, path: &[u8]) -> bool {
        self.host.check_open_basedir(path)
    }
    fn realpath(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        self.host.realpath(path)
    }
}

/// A host for code paths without a VM (object destructors): diagnostics
/// are dropped and user code cannot be called.
#[derive(Debug, Default)]
pub struct NullHost {
    persistent: PersistentList,
}

impl RuntimeContext for NullHost {
    fn diagnostic(&mut self, _: ErrorLevel, _: &str) -> RtResult<()> {
        Ok(())
    }
}

impl PdoHost for NullHost {
    fn call(&mut self, _: &Value, _: &[Value]) -> RtResult<Value> {
        Err(RuntimeError::Error("No VM available".into()))
    }
    fn check_callable(&mut self, _: &Value) -> Result<(), String> {
        Err("no VM available".into())
    }
    fn lookup_class(&mut self, _: &ZStr) -> Option<ClassInfo> {
        None
    }
    fn instance_of(&mut self, class: &ZStr, parent: &str) -> bool {
        class.as_bytes().eq_ignore_ascii_case(parent.as_bytes())
    }
    fn instantiate(&mut self, class: &ZStr) -> RtResult<Value> {
        Err(RuntimeError::Error(format!("Cannot instantiate {class}")))
    }
    fn call_constructor(&mut self, _: &Value, _: Option<&ZArray>) -> RtResult<()> {
        Ok(())
    }
    fn update_property(&mut self, _: &Value, _: Option<&ZStr>, _: &ZStr, _: Value) -> RtResult<()> {
        Ok(())
    }
    fn instantiate_statement(&mut self, class: &ZStr) -> RtResult<(Value, PdoStatement)> {
        Err(RuntimeError::Error(format!("Cannot instantiate {class}")))
    }
    fn instantiate_pdo(&mut self, class: &ZStr) -> RtResult<(Value, Pdo)> {
        Err(RuntimeError::Error(format!("Cannot instantiate {class}")))
    }
    fn std_write_property(&mut self, _: &Value, _: &ZStr, _: Value) -> RtResult<()> {
        Ok(())
    }
    fn create_row(&mut self, _: PdoRow) -> RtResult<Value> {
        Ok(Value::Null)
    }
    fn write_output(&mut self, _: &[u8]) -> RtResult<()> {
        Ok(())
    }
    fn persistent_list(&mut self) -> &mut PersistentList {
        &mut self.persistent
    }
}

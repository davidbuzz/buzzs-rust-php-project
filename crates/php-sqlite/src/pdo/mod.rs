// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The SQLite driver of PDO (`ext/pdo_sqlite`), shaped after the
//! `pdo_dbh_methods` / `pdo_stmt_methods` tables the PDO core calls.
//!
//! The PDO core (in the VM) owns the generic state — error mode, the
//! `errorCode` SQLSTATEs, `executed`, fetch modes, bound-parameter hashes —
//! and calls into [`PdoSqliteDbh`] / [`PdoSqliteStmt`]:
//!
//! | C method                         | Rust                                   |
//! |----------------------------------|----------------------------------------|
//! | `pdo_sqlite_handle_factory`      | [`PdoSqliteDbh::connect`]              |
//! | `sqlite_handle_closer`           | [`PdoSqliteDbh::close`]                |
//! | `sqlite_handle_preparer`         | [`PdoSqliteDbh::prepare`]              |
//! | `sqlite_handle_doer`             | [`PdoSqliteDbh::exec`]                 |
//! | `sqlite_handle_quoter`           | [`PdoSqliteDbh::quote`]                |
//! | `sqlite_handle_begin/commit/rollback` | [`PdoSqliteDbh::begin`] …         |
//! | `pdo_sqlite_set_attr` / `get_attribute` | [`PdoSqliteDbh::set_attribute`] … |
//! | `pdo_sqlite_last_insert_id`      | [`PdoSqliteDbh::last_insert_id`]       |
//! | `pdo_sqlite_fetch_error_func`    | [`PdoSqliteDbh::fetch_error`]          |
//! | `pdo_sqlite_in_transaction`      | [`PdoSqliteDbh::in_transaction`]       |
//! | `pdo_sqlite_scanner`             | [`parser::SqliteScanner`]              |
//! | `pdo_sqlite_stmt_*`              | [`PdoSqliteStmt`] methods              |
//!
//! Driver failures are reported as `Err(SqlState)`: the SQLSTATE the C
//! driver writes into `dbh->error_code` / `stmt->error_code`; the native
//! code and message are then available from [`PdoSqliteDbh::fetch_error`].
//! Exceptions raised by user callbacks or conversions are left pending in
//! the [`Cx`] the core passes in, exactly like `EG(exception)`.

pub mod parser;
pub mod sqlstate;

use crate::blob::{BlobFlavor, BlobStream};
use crate::constants::{
    PDO_SQLITE_ATTR_BUSY_STATEMENT, PDO_SQLITE_ATTR_EXPLAIN_STATEMENT,
    PDO_SQLITE_ATTR_EXTENDED_RESULT_CODES, PDO_SQLITE_ATTR_OPEN_FLAGS,
    PDO_SQLITE_ATTR_READONLY_STATEMENT, PDO_SQLITE_ATTR_TRANSACTION_MODE,
    PDO_SQLITE_TRANSACTION_MODE_DEFERRED, PDO_SQLITE_TRANSACTION_MODE_EXCLUSIVE,
    PDO_SQLITE_TRANSACTION_MODE_IMMEDIATE, SQLITE3_OPEN_CREATE, SQLITE3_OPEN_READWRITE,
};
use crate::conv::{arg_to_value, failed_to_invoke, nullable_str, set_slot, value_to_result};
use crate::error::{ExceptionClass, PhpError};
use crate::ffi::{
    self, AggregateFunction, AggregateState, ArgValue, AuthorizerFunction, CollationFunction,
    Connection, FnResult, RawStatement, ScalarFunction, UserFunction, SQLITE_ATTACH, SQLITE_BLOB,
    SQLITE_CONSTRAINT, SQLITE_DENY, SQLITE_DONE, SQLITE_ERROR, SQLITE_FLOAT, SQLITE_IGNORE,
    SQLITE_INTEGER, SQLITE_INTERRUPT, SQLITE_NOLFS, SQLITE_NOTFOUND, SQLITE_NULL, SQLITE_OK,
    SQLITE_OPEN_URI, SQLITE_ROW, SQLITE_TEXT, SQLITE_TOOBIG, SQLITE_UTF8,
};
use crate::host::Cx;
use php_runtime::numeric::{is_numeric_string, Number};
use php_runtime::{ArrayKey, HashTable, PhpRef, Value, ZArray, ZStr};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub use parser::{PARAM_BOOL, PARAM_INT, PARAM_LOB, PARAM_NULL, PARAM_STMT, PARAM_STR};

/// `PDO_ATTR_TIMEOUT`.
pub const ATTR_TIMEOUT: i64 = 2;
/// `PDO_ATTR_SERVER_VERSION`.
pub const ATTR_SERVER_VERSION: i64 = 4;
/// `PDO_ATTR_CLIENT_VERSION`.
pub const ATTR_CLIENT_VERSION: i64 = 5;
/// `PDO_ATTR_CURSOR`.
pub const ATTR_CURSOR: i64 = 10;
/// `PDO_CURSOR_FWDONLY`.
pub const CURSOR_FWDONLY: i64 = 0;
/// `PDO_ERRMODE_SILENT`.
pub const ERRMODE_SILENT: i64 = 0;
/// `PDO_ERRMODE_WARNING`.
pub const ERRMODE_WARNING: i64 = 1;
/// `PDO_ERRMODE_EXCEPTION`.
pub const ERRMODE_EXCEPTION: i64 = 2;

/// `PDO_PARAM_TYPE(x)`: the type without the `PDO_PARAM_*` flag bits.
pub fn param_type_base(x: i64) -> i64 {
    x & 0xFFFF
}

/// A SQLSTATE (`pdo_error_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SqlState(pub [u8; 5]);

impl SqlState {
    /// `PDO_ERR_NONE`.
    pub const NONE: SqlState = SqlState(*b"00000");

    /// The SQLSTATE text.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("HY000")
    }

    /// `pdo_sqlstate_state_to_description()`.
    pub fn description(&self) -> Option<&'static str> {
        sqlstate::description(self.as_str())
    }
}

impl std::fmt::Display for SqlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The SQLSTATE `_pdo_sqlite_error()` maps a SQLite result code to.
pub fn sqlstate_for(errcode: i32) -> SqlState {
    SqlState(*match errcode {
        SQLITE_OK => b"00000",
        SQLITE_NOTFOUND => b"42S02",
        SQLITE_INTERRUPT => b"01002",
        SQLITE_NOLFS => b"HYC00",
        SQLITE_TOOBIG => b"22001",
        SQLITE_CONSTRAINT => b"23000",
        _ => b"HY000",
    })
}

/// `pdo_sqlite_error_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorInfo {
    /// The native error code (0 when none).
    pub errcode: i32,
    /// The native message.
    pub errmsg: Option<Vec<u8>>,
}

/// How a connection attempt failed.
#[derive(Debug, Clone)]
pub enum ConnectError {
    /// An exception to throw as is (`open_basedir prohibits opening ...`).
    Exception(PhpError),
    /// `pdo_throw_exception(errcode, errmsg, sqlstate)`: a `PDOException`
    /// with message `SQLSTATE[%s] [%d] %s`, code `errcode` and
    /// `errorInfo = [sqlstate, errcode, errmsg]`.
    Driver {
        /// The SQLSTATE.
        sqlstate: SqlState,
        /// The native code.
        code: i32,
        /// The native message.
        message: Vec<u8>,
    },
}

impl ConnectError {
    /// The exception message the PDO core produces.
    pub fn message(&self) -> String {
        match self {
            ConnectError::Exception(e) => e.message().unwrap_or_default().to_string(),
            ConnectError::Driver {
                sqlstate,
                code,
                message,
            } => {
                format!(
                    "SQLSTATE[{sqlstate}] [{code}] {}",
                    String::from_utf8_lossy(message)
                )
            }
        }
    }
}

/// `pdo_get_long_param()`: `None` after raising the `TypeError`.
pub fn get_long_param(cx: &mut Cx<'_>, v: &Value) -> Option<i64> {
    match v.deref() {
        Value::Int(i) => Some(i),
        Value::Bool(b) => Some(i64::from(b)),
        Value::Str(s) => {
            if let Some(Number::Int(i)) = is_numeric_string(s.as_bytes(), true) {
                return Some(i);
            }
            long_param_error(cx, v);
            None
        }
        _ => {
            long_param_error(cx, v);
            None
        }
    }
}

fn long_param_error(cx: &mut Cx<'_>, v: &Value) {
    cx.throw_new(
        ExceptionClass::TypeError,
        format!(
            "Attribute value must be of type int for selected attribute, {} given",
            v.value_name()
        ),
        0,
    );
}

/// `pdo_attr_lval(options, option, default)`.
fn attr_lval(cx: &mut Cx<'_>, options: Option<&ZArray>, option: i64, default: i64) -> i64 {
    match options.and_then(|o| o.get_int(option)) {
        Some(v) => {
            let r = v.to_long(cx.host());
            cx.check(r).unwrap_or(0)
        }
        None => default,
    }
}

/// `make_filename_safe()`: `None` when `open_basedir` forbids the file.
fn make_filename_safe(cx: &mut Cx<'_>, filename: Option<&[u8]>) -> Option<Vec<u8>> {
    let filename = filename?;
    if !filename.is_empty() && filename.len() >= 5 && filename[..5].eq_ignore_ascii_case(b"file:") {
        if cx.host().open_basedir_active() {
            return None;
        }
        return Some(filename.to_vec());
    }
    if !filename.is_empty() && filename != b":memory:" {
        let full = cx.host().expand_filepath(filename)?;
        if cx.host().check_open_basedir(&full) {
            return None;
        }
        return Some(full);
    }
    Some(filename.to_vec())
}

// ---------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct PdoShared {
    authorizer: RefCell<Option<Value>>,
}

struct Scalar {
    callable: Value,
}

impl ScalarFunction for Scalar {
    fn call(&self, args: Vec<ArgValue>) -> FnResult {
        ffi::with_scope(|cx| {
            let Some(cx) = cx else {
                return failed_to_invoke();
            };
            let zargs: Vec<Value> = args.into_iter().map(arg_to_value).collect();
            match cx.call(&self.callable, &zargs) {
                Some(v) => value_to_result(cx, &v),
                None => failed_to_invoke(),
            }
        })
    }
}

struct Aggregate {
    step: Value,
    fini: Value,
}

/// `do_callback(..., is_agg)`: the context is passed by reference; the
/// outer `None` means no call context, the inner one an `UNDEF` result.
fn invoke_aggregate(callable: &Value, state: &mut AggregateState, args: Vec<ArgValue>) -> Invoked {
    let ctx_ref = if let Some(Value::Reference(r)) = &state.context {
        r.clone()
    } else {
        let r = PhpRef::new(Value::Null);
        state.context = Some(Value::Reference(r.clone()));
        r
    };
    state.rows += 1;
    let rows = state.rows;
    ffi::with_scope(|cx| {
        let Some(cx) = cx else {
            return Invoked::NoScope;
        };
        let mut zargs = Vec::with_capacity(args.len() + 2);
        zargs.push(Value::Reference(ctx_ref));
        zargs.push(Value::Int(rows));
        zargs.extend(args.into_iter().map(arg_to_value));
        Invoked::Returned(cx.call(callable, &zargs))
    })
}

/// Outcome of invoking an aggregate callback.
enum Invoked {
    /// No call context.
    NoScope,
    /// The return value (`None` is `UNDEF`).
    Returned(Option<Value>),
}

impl AggregateFunction for Aggregate {
    fn step(&self, state: &mut AggregateState, args: Vec<ArgValue>) -> FnResult {
        if let Invoked::Returned(Some(ret)) = invoke_aggregate(&self.step, state, args) {
            if let Some(Value::Reference(r)) = &state.context {
                r.set(ret.deref());
            }
        }
        FnResult::Unset
    }

    fn finalize(&self, state: &mut AggregateState) -> FnResult {
        let r = match invoke_aggregate(&self.fini, state, Vec::new()) {
            Invoked::Returned(Some(v)) => ffi::with_scope(|cx| match cx {
                Some(cx) => value_to_result(cx, &v),
                None => FnResult::Unset,
            }),
            _ => failed_to_invoke(),
        };
        state.context = None;
        r
    }
}

struct Collation {
    callable: Value,
}

impl CollationFunction for Collation {
    fn compare(&self, a: &[u8], b: &[u8]) -> i32 {
        ffi::with_scope(|cx| {
            let Some(cx) = cx else {
                return 0;
            };
            match cx.call(
                &self.callable,
                &[Value::Str(ZStr::from(a)), Value::Str(ZStr::from(b))],
            ) {
                Some(Value::Int(i)) => match i.cmp(&0) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                },
                Some(other) => {
                    let func = cx.host().active_function_name();
                    cx.throw_new(
                        ExceptionClass::TypeError,
                        format!(
                            "{func}(): Return value of the collation callback must be of type int, {} returned",
                            other.value_name()
                        ),
                        0,
                    );
                    -1
                }
                None => 0,
            }
        })
    }
}

struct Authorizer {
    shared: Rc<PdoShared>,
}

impl AuthorizerFunction for Authorizer {
    fn authorize(&self, action: i32, args: [Option<&[u8]>; 4]) -> i32 {
        ffi::with_scope(|cx| {
            let callable = self.shared.authorizer.borrow().clone();
            let Some(cx) = cx else {
                return if callable.is_some() {
                    SQLITE_DENY
                } else {
                    SQLITE_OK
                };
            };
            if cx.host().open_basedir_active()
                && action == SQLITE_ATTACH
                && make_filename_safe(cx, args[0]).is_none()
            {
                return SQLITE_DENY;
            }
            let Some(callable) = callable else {
                return SQLITE_OK;
            };
            let argv = [
                Value::Int(i64::from(action)),
                nullable_str(args[0]),
                nullable_str(args[1]),
                nullable_str(args[2]),
                nullable_str(args[3]),
            ];
            match cx.call(&callable, &argv) {
                None => SQLITE_DENY,
                Some(Value::Int(v)) => {
                    if v == i64::from(SQLITE_OK)
                        || v == i64::from(SQLITE_IGNORE)
                        || v == i64::from(SQLITE_DENY)
                    {
                        #[allow(clippy::cast_possible_truncation)]
                        return v as i32;
                    }
                    let func = cx.host().active_function_name();
                    cx.throw_new(
                        ExceptionClass::ValueError,
                        format!("{func}(): Return value of the authorizer callback must be one of Pdo\\Sqlite::OK, Pdo\\Sqlite::DENY, or Pdo\\Sqlite::IGNORE"),
                        0,
                    );
                    SQLITE_DENY
                }
                Some(other) => {
                    let func = cx.host().active_function_name();
                    cx.throw_new(
                        ExceptionClass::TypeError,
                        format!(
                            "{func}(): Return value of the authorizer callback must be of type int, {} returned",
                            other.value_name()
                        ),
                        0,
                    );
                    SQLITE_DENY
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Database handle
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Handle {
    conn: Rc<Connection>,
    einfo: RefCell<ErrorInfo>,
    shared: Rc<PdoShared>,
    transaction_mode: Cell<i64>,
    funcs: RefCell<Vec<(Vec<u8>, i64)>>,
    collations: RefCell<Vec<Vec<u8>>>,
}

impl Handle {
    /// `_pdo_sqlite_error()`: records the native error and returns the
    /// SQLSTATE.
    fn error(&self) -> SqlState {
        let code = self.conn.errcode();
        let mut einfo = self.einfo.borrow_mut();
        einfo.errcode = code;
        if code == SQLITE_OK {
            return SqlState::NONE;
        }
        einfo.errmsg = Some(self.conn.errmsg());
        sqlstate_for(code)
    }

    fn cleanup_callbacks(&self) {
        self.shared.authorizer.borrow_mut().take();
        for (name, argc) in self.funcs.borrow_mut().drain(..) {
            self.conn
                .create_function(&name, argc, i64::from(SQLITE_UTF8), None);
        }
        for name in self.collations.borrow_mut().drain(..) {
            self.conn.create_collation(&name, None);
        }
    }
}

/// The driver data of a SQLite PDO connection (`pdo_sqlite_db_handle`).
#[derive(Debug, Clone)]
pub struct PdoSqliteDbh {
    h: Rc<Handle>,
}

/// The column description from `describer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdoColumn {
    /// `name`.
    pub name: ZStr,
    /// `maxlen` (always -1).
    pub maxlen: i64,
    /// `precision` (always 0).
    pub precision: i64,
}

impl PdoSqliteDbh {
    /// `dbh->alloc_own_columns`.
    pub const ALLOC_OWN_COLUMNS: bool = true;
    /// `dbh->max_escaped_char_length`.
    pub const MAX_ESCAPED_CHAR_LENGTH: u8 = 2;
    /// `dbh->skip_param_evt`: every event but `PDO_PARAM_EVT_EXEC_PRE`.
    pub const SKIP_PARAM_EVT: u8 = 0x7F ^ (1 << 2);
    /// `stmt->supports_placeholders` of prepared statements.
    pub const SUPPORTS_PLACEHOLDERS: u8 =
        parser::PLACEHOLDER_NAMED | parser::PLACEHOLDER_POSITIONAL;

    /// `pdo_sqlite_handle_factory()`. `data_source` is the DSN without the
    /// `sqlite:` prefix; `options` the driver options array.
    pub fn connect(
        cx: &mut Cx<'_>,
        data_source: &[u8],
        options: Option<&ZArray>,
    ) -> Result<Self, ConnectError> {
        let Some(filename) = make_filename_safe(cx, Some(data_source)) else {
            return Err(ConnectError::Exception(PhpError::new(
                ExceptionClass::PdoException,
                format!(
                    "open_basedir prohibits opening {}",
                    String::from_utf8_lossy(data_source)
                ),
                0,
            )));
        };
        let mut flags = attr_lval(
            cx,
            options,
            PDO_SQLITE_ATTR_OPEN_FLAGS,
            SQLITE3_OPEN_READWRITE | SQLITE3_OPEN_CREATE,
        );
        if !cx.host().open_basedir_active() {
            flags |= i64::from(SQLITE_OPEN_URI);
        }
        #[allow(clippy::cast_possible_truncation)]
        let conn = match Connection::open(&filename, flags as i32) {
            Ok(c) => c,
            Err(e) => {
                return Err(ConnectError::Driver {
                    sqlstate: sqlstate_for(e.code),
                    code: e.code,
                    message: e.message,
                });
            }
        };
        let shared = Rc::new(PdoShared::default());
        conn.set_authorizer(Box::new(Authorizer {
            shared: Rc::clone(&shared),
        }));
        let timeout = if options.is_some() {
            attr_lval(cx, options, ATTR_TIMEOUT, 60)
        } else {
            60
        };
        conn.busy_timeout(timeout.wrapping_mul(1000));
        Ok(PdoSqliteDbh {
            h: Rc::new(Handle {
                conn,
                einfo: RefCell::new(ErrorInfo::default()),
                shared,
                transaction_mode: Cell::new(PDO_SQLITE_TRANSACTION_MODE_DEFERRED),
                funcs: RefCell::new(Vec::new()),
                collations: RefCell::new(Vec::new()),
            }),
        })
    }

    /// Splits a DSN into driver name and data source (`"sqlite::memory:"` →
    /// `("sqlite", ":memory:")`); `None` without a colon.
    pub fn split_dsn(dsn: &[u8]) -> Option<(&[u8], &[u8])> {
        let colon = dsn.iter().position(|&b| b == b':')?;
        Some((&dsn[..colon], &dsn[colon + 1..]))
    }

    /// Whether two handles share the same connection.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.h, &b.h)
    }

    /// `sqlite_handle_closer()` / `pdo_sqlite_request_shutdown()`:
    /// unregisters the user callbacks (the connection itself closes when
    /// the last statement releases it).
    pub fn close(&self) {
        self.h.cleanup_callbacks();
    }

    /// The current native error (`einfo`).
    pub fn error_info(&self) -> ErrorInfo {
        self.h.einfo.borrow().clone()
    }

    /// `pdo_sqlite_fetch_error_func()`: `[errcode, errmsg]` when an error
    /// was recorded, else empty.
    pub fn fetch_error(&self) -> Vec<Value> {
        let e = self.h.einfo.borrow();
        if e.errcode == 0 {
            return Vec::new();
        }
        vec![
            Value::Int(i64::from(e.errcode)),
            Value::Str(ZStr::from(e.errmsg.clone().unwrap_or_default())),
        ]
    }

    /// Records a driver error and returns its SQLSTATE (`pdo_sqlite_error`).
    pub fn raise_error(&self) -> SqlState {
        self.h.error()
    }

    /// `sqlite_handle_preparer()`. `cursor` is the `PDO::ATTR_CURSOR`
    /// driver option (only forward-only cursors are supported).
    pub fn prepare(
        &self,
        cx: &mut Cx<'_>,
        sql: &[u8],
        cursor: i64,
    ) -> Result<PdoSqliteStmt, SqlState> {
        if cursor != CURSOR_FWDONLY {
            self.h.einfo.borrow_mut().errcode = SQLITE_ERROR;
            return Err(self.h.error());
        }
        let conn = Rc::clone(&self.h.conn);
        match ffi::enter(cx, || conn.prepare(sql)) {
            Ok(raw) => Ok(PdoSqliteStmt {
                h: Rc::clone(&self.h),
                stmt: Rc::new(raw),
                pre_fetched: Cell::new(false),
                done: Cell::new(false),
            }),
            Err(_) => Err(self.h.error()),
        }
    }

    /// `sqlite_handle_doer()`: the number of changed rows.
    pub fn exec(&self, cx: &mut Cx<'_>, sql: &[u8]) -> Result<i64, SqlState> {
        let conn = Rc::clone(&self.h.conn);
        match ffi::enter(cx, || conn.exec(sql)) {
            Ok(()) => Ok(conn.changes()),
            Err(_) => Err(self.h.error()),
        }
    }

    /// `sqlite_handle_quoter()`: `None` on failure. Strings with NUL bytes
    /// are rejected according to `error_mode` (exception, warning or
    /// silently).
    pub fn quote(
        &self,
        cx: &mut Cx<'_>,
        unquoted: &[u8],
        paramtype: i64,
        error_mode: i64,
    ) -> Option<ZStr> {
        let _ = paramtype;
        if unquoted.len() > (i32::MAX as usize - 3) / 2 {
            return None;
        }
        if unquoted.contains(&0) {
            let msg = "SQLite PDO::quote does not support null bytes";
            if error_mode == ERRMODE_EXCEPTION {
                cx.throw_new(ExceptionClass::PdoException, msg, 0);
            } else if error_mode == ERRMODE_WARNING {
                cx.warning(msg);
            }
            return None;
        }
        let mut out = Vec::with_capacity(unquoted.len() + 2);
        out.push(b'\'');
        out.extend_from_slice(&ffi::escape_q(unquoted).unwrap_or_default());
        out.push(b'\'');
        Some(ZStr::from(out))
    }

    fn txn(&self, cx: &mut Cx<'_>, sql: &[u8]) -> Result<(), SqlState> {
        let conn = Rc::clone(&self.h.conn);
        match ffi::enter(cx, || conn.exec(sql)) {
            Ok(()) => Ok(()),
            Err(_) => Err(self.h.error()),
        }
    }

    /// `sqlite_handle_begin()` (honours `Pdo\Sqlite::ATTR_TRANSACTION_MODE`).
    pub fn begin(&self, cx: &mut Cx<'_>) -> Result<(), SqlState> {
        let sql: &[u8] = match self.h.transaction_mode.get() {
            PDO_SQLITE_TRANSACTION_MODE_DEFERRED => b"BEGIN DEFERRED TRANSACTION",
            PDO_SQLITE_TRANSACTION_MODE_IMMEDIATE => b"BEGIN IMMEDIATE TRANSACTION",
            PDO_SQLITE_TRANSACTION_MODE_EXCLUSIVE => b"BEGIN EXCLUSIVE TRANSACTION",
            _ => b"BEGIN",
        };
        self.txn(cx, sql)
    }

    /// `sqlite_handle_commit()`.
    pub fn commit(&self, cx: &mut Cx<'_>) -> Result<(), SqlState> {
        self.txn(cx, b"COMMIT")
    }

    /// `sqlite_handle_rollback()`.
    pub fn rollback(&self, cx: &mut Cx<'_>) -> Result<(), SqlState> {
        self.txn(cx, b"ROLLBACK")
    }

    /// `pdo_sqlite_in_transaction()`.
    pub fn in_transaction(&self) -> bool {
        !self.h.conn.get_autocommit()
    }

    /// `pdo_sqlite_set_attr()`: `false` for unsupported attributes or
    /// invalid values (a `TypeError` may be pending).
    pub fn set_attribute(&self, cx: &mut Cx<'_>, attr: i64, val: &Value) -> bool {
        match attr {
            ATTR_TIMEOUT => {
                let Some(l) = get_long_param(cx, val) else {
                    return false;
                };
                self.h.conn.busy_timeout(l.wrapping_mul(1000));
                true
            }
            PDO_SQLITE_ATTR_EXTENDED_RESULT_CODES => {
                let Some(l) = get_long_param(cx, val) else {
                    return false;
                };
                self.h.conn.extended_result_codes(l);
                true
            }
            PDO_SQLITE_ATTR_TRANSACTION_MODE => {
                let Some(l) = get_long_param(cx, val) else {
                    return false;
                };
                if (PDO_SQLITE_TRANSACTION_MODE_DEFERRED..=PDO_SQLITE_TRANSACTION_MODE_EXCLUSIVE)
                    .contains(&l)
                {
                    self.h.transaction_mode.set(l);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// `pdo_sqlite_get_attribute()`: `None` for attributes the driver does
    /// not handle.
    pub fn get_attribute(&self, attr: i64) -> Option<Value> {
        match attr {
            ATTR_CLIENT_VERSION | ATTR_SERVER_VERSION => Some(Value::from(ffi::libversion())),
            PDO_SQLITE_ATTR_TRANSACTION_MODE => Some(Value::Int(self.h.transaction_mode.get())),
            _ => None,
        }
    }

    /// `pdo_sqlite_last_insert_id()`.
    pub fn last_insert_id(&self) -> ZStr {
        ZStr::from(self.h.conn.last_insert_rowid().to_string())
    }

    /// `createFunction()` / `sqliteCreateFunction()`.
    pub fn create_function(&self, name: &[u8], callback: Value, argc: i64, flags: i64) -> bool {
        let f = UserFunction::Scalar(Box::new(Scalar { callable: callback }));
        let ok = self
            .h
            .conn
            .create_function(name, argc, flags | i64::from(SQLITE_UTF8), Some(f))
            == SQLITE_OK;
        if ok {
            self.h.funcs.borrow_mut().push((name.to_vec(), argc));
        }
        ok
    }

    /// `createAggregate()` / `sqliteCreateAggregate()`.
    pub fn create_aggregate(&self, name: &[u8], step: Value, finalize: Value, argc: i64) -> bool {
        let f = UserFunction::Aggregate(Box::new(Aggregate {
            step,
            fini: finalize,
        }));
        let ok = self
            .h
            .conn
            .create_function(name, argc, i64::from(SQLITE_UTF8), Some(f))
            == SQLITE_OK;
        if ok {
            self.h.funcs.borrow_mut().push((name.to_vec(), argc));
        }
        ok
    }

    /// `createCollation()` / `sqliteCreateCollation()`.
    pub fn create_collation(&self, name: &[u8], callback: Value) -> bool {
        let ok = self
            .h
            .conn
            .create_collation(name, Some(Box::new(Collation { callable: callback })))
            == SQLITE_OK;
        if ok {
            self.h.collations.borrow_mut().push(name.to_vec());
        }
        ok
    }

    /// `Pdo\Sqlite::setAuthorizer()` (`None` disables it).
    pub fn set_authorizer(&self, callback: Option<Value>) {
        *self.h.shared.authorizer.borrow_mut() = callback;
    }

    /// `Pdo\Sqlite::loadExtension()`.
    pub fn load_extension(&self, cx: &mut Cx<'_>, name: &[u8]) -> Result<(), PhpError> {
        if name.is_empty() {
            return Err(PhpError::new(
                ExceptionClass::ValueError,
                "Pdo\\Sqlite::loadExtension(): Argument #1 ($name) must not be empty",
                0,
            ));
        }
        let Some(full) = cx.host().realpath(name) else {
            return Err(PhpError::new(
                ExceptionClass::PdoException,
                format!(
                    "Unable to load extension \"{}\"",
                    String::from_utf8_lossy(name)
                ),
                0,
            ));
        };
        self.h.conn.load_extension(&full, true).map_err(|msg| {
            PhpError::new(
                ExceptionClass::PdoException,
                format!(
                    "Unable to load extension \"{}\"",
                    String::from_utf8_lossy(&msg.unwrap_or_default())
                ),
                0,
            )
        })
    }

    /// `Pdo\Sqlite::openBlob()`: `None` is `false` after the (unprefixed)
    /// "Unable to open blob" warning.
    pub fn open_blob(
        &self,
        cx: &mut Cx<'_>,
        table: &[u8],
        column: &[u8],
        rowid: i64,
        dbname: &[u8],
        flags: i64,
    ) -> Option<BlobStream> {
        let writable = flags & SQLITE3_OPEN_READWRITE != 0;
        if let Ok(b) = self
            .h
            .conn
            .blob_open(dbname, table, column, rowid, writable)
        {
            Some(BlobStream::new(b, flags, writable, BlobFlavor::Pdo))
        } else {
            let msg = format!(
                "Unable to open blob: {}",
                String::from_utf8_lossy(&self.h.conn.errmsg())
            );
            cx.error(php_runtime::ErrorLevel::Warning, &msg);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

/// `enum pdo_param_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamEvent {
    /// `PDO_PARAM_EVT_ALLOC`.
    Alloc,
    /// `PDO_PARAM_EVT_FREE`.
    Free,
    /// `PDO_PARAM_EVT_EXEC_PRE`.
    ExecPre,
    /// `PDO_PARAM_EVT_EXEC_POST`.
    ExecPost,
    /// `PDO_PARAM_EVT_FETCH_PRE`.
    FetchPre,
    /// `PDO_PARAM_EVT_FETCH_POST`.
    FetchPost,
    /// `PDO_PARAM_EVT_NORMALIZE`.
    Normalize,
}

/// `struct pdo_bound_param_data` (the fields the driver uses).
#[derive(Debug, Clone)]
pub struct PdoBoundParam {
    /// The variable (a [`Value::Reference`] for `bindParam()`).
    pub parameter: Value,
    /// 0-based position, `-1` while only the name is known.
    pub paramno: i64,
    /// The name including its leading `:`.
    pub name: Option<ZStr>,
    /// `PDO::PARAM_*` (possibly with flags).
    pub param_type: i64,
    /// Parameter (`true`) or bound column (`false`).
    pub is_param: bool,
}

/// Why a parameter hook failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookError {
    /// A driver error (`pdo_sqlite_error_stmt`) with its SQLSTATE.
    Driver(SqlState),
    /// `pdo_raise_impl_error(dbh, stmt, sqlstate, message)`.
    Impl(&'static str, &'static str),
    /// Failure without a new error code (an exception may be pending).
    Silent,
}

/// What `executer` reports to the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecuteInfo {
    /// `php_pdo_stmt_set_column_count()`.
    pub column_count: i64,
    /// `stmt->row_count` when set (statement finished).
    pub row_count: Option<i64>,
}

/// A prepared SQLite PDO statement (`pdo_sqlite_stmt`).
#[derive(Debug)]
pub struct PdoSqliteStmt {
    h: Rc<Handle>,
    stmt: Rc<RawStatement>,
    pre_fetched: Cell<bool>,
    done: Cell<bool>,
}

impl PdoSqliteStmt {
    /// `pdo_sqlite_stmt_execute()`. `executed` is `stmt->executed`.
    pub fn execute(&self, cx: &mut Cx<'_>, executed: bool) -> Result<ExecuteInfo, SqlState> {
        if executed && !self.done.get() {
            self.stmt.reset();
        }
        self.done.set(false);
        let stmt = Rc::clone(&self.stmt);
        match ffi::enter(cx, || stmt.step()) {
            SQLITE_ROW => {
                self.pre_fetched.set(true);
                Ok(ExecuteInfo {
                    column_count: i64::from(stmt.data_count()),
                    row_count: None,
                })
            }
            SQLITE_DONE => {
                let info = ExecuteInfo {
                    column_count: i64::from(stmt.column_count()),
                    row_count: Some(self.h.conn.changes()),
                };
                stmt.reset();
                self.done.set(true);
                Ok(info)
            }
            _ => {
                stmt.reset();
                Err(self.h.error())
            }
        }
    }

    /// `pdo_sqlite_stmt_fetch()`: `Ok(false)` when there are no more rows.
    pub fn fetch(&self, cx: &mut Cx<'_>) -> Result<bool, SqlState> {
        if self.stmt.is_null() {
            return Ok(false);
        }
        if self.pre_fetched.get() {
            self.pre_fetched.set(false);
            return Ok(true);
        }
        if self.done.get() {
            return Ok(false);
        }
        let stmt = Rc::clone(&self.stmt);
        match ffi::enter(cx, || stmt.step()) {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => {
                self.done.set(true);
                stmt.reset();
                Ok(false)
            }
            rc => {
                if rc == SQLITE_ERROR {
                    stmt.reset();
                }
                Err(self.h.error())
            }
        }
    }

    /// `pdo_sqlite_stmt_describe()`.
    pub fn describe(&self, colno: i64) -> Result<PdoColumn, SqlState> {
        if colno >= i64::from(self.stmt.column_count()) {
            return Err(self.h.error());
        }
        #[allow(clippy::cast_possible_truncation)]
        let name = self.stmt.column_name(colno as i32).unwrap_or_default();
        Ok(PdoColumn {
            name: ZStr::from(name),
            maxlen: -1,
            precision: 0,
        })
    }

    /// `pdo_sqlite_stmt_get_col()`: native types (`int`, `float`, string,
    /// `null`).
    pub fn get_col(&self, colno: i64) -> Result<Value, SqlState> {
        if self.stmt.is_null() {
            return Ok(Value::Null);
        }
        if colno >= i64::from(self.stmt.data_count()) {
            return Err(self.h.error());
        }
        #[allow(clippy::cast_possible_truncation)]
        let col = colno as i32;
        Ok(match self.stmt.column_type(col) {
            SQLITE_NULL => Value::Null,
            SQLITE_INTEGER => Value::Int(self.stmt.column_int64(col)),
            SQLITE_FLOAT => Value::Float(self.stmt.column_double(col)),
            SQLITE_BLOB => Value::Str(ZStr::from(self.stmt.column_blob(col).unwrap_or_default())),
            _ => match self.stmt.column_text(col) {
                Some(t) => Value::Str(ZStr::from(t)),
                None => return Err(self.h.error()),
            },
        })
    }

    /// `pdo_sqlite_stmt_param_hook()`. `executed` is `stmt->executed`.
    pub fn param_hook(
        &self,
        cx: &mut Cx<'_>,
        param: &mut PdoBoundParam,
        event: ParamEvent,
        executed: bool,
    ) -> Result<(), HookError> {
        if event != ParamEvent::ExecPre {
            return Ok(());
        }
        if executed && !self.done.get() {
            self.stmt.reset();
            self.done.set(true);
        }
        if !param.is_param {
            return Ok(());
        }
        if param.paramno == -1 {
            let name = param.name.clone().unwrap_or_else(ZStr::empty);
            param.paramno = i64::from(self.stmt.bind_parameter_index(name.as_bytes())) - 1;
        }
        let idx = param.paramno + 1;
        let driver_err = |s: &Self| Err(HookError::Driver(s.h.error()));
        match param_type_base(param.param_type) {
            PARAM_STMT => Err(HookError::Silent),
            PARAM_NULL => {
                if self.stmt.bind_null(idx) == SQLITE_OK {
                    Ok(())
                } else {
                    driver_err(self)
                }
            }
            PARAM_INT | PARAM_BOOL => {
                let v = param.parameter.deref();
                let rc = if v.is_null() {
                    self.stmt.bind_null(idx)
                } else {
                    let r = v.to_long(cx.host());
                    let l = cx.check(r).unwrap_or(0);
                    set_slot(&mut param.parameter, Value::Int(l));
                    self.stmt.bind_int(idx, l)
                };
                if rc == SQLITE_OK {
                    Ok(())
                } else {
                    driver_err(self)
                }
            }
            PARAM_LOB => {
                let v = param.parameter.deref();
                let bytes = match &v {
                    Value::Resource(_) => match cx.host().read_stream(&v) {
                        Some(mem) => {
                            set_slot(&mut param.parameter, Value::Str(ZStr::from(mem.clone())));
                            mem
                        }
                        None => return Err(HookError::Impl("HY105", "Expected a stream resource")),
                    },
                    Value::Null => {
                        return if self.stmt.bind_null(idx) == SQLITE_OK {
                            Ok(())
                        } else {
                            driver_err(self)
                        };
                    }
                    _ => {
                        let r = v.to_zstr(cx.host());
                        let Some(s) = cx.check(r) else {
                            return Err(HookError::Silent);
                        };
                        set_slot(&mut param.parameter, Value::Str(s.clone()));
                        s.into_bytes()
                    }
                };
                if self.stmt.bind_blob(idx, &bytes) == SQLITE_OK {
                    Ok(())
                } else {
                    Err(HookError::Silent)
                }
            }
            _ => {
                let v = param.parameter.deref();
                let rc = if v.is_null() {
                    self.stmt.bind_null(idx)
                } else {
                    let r = v.to_zstr(cx.host());
                    let Some(s) = cx.check(r) else {
                        return Err(HookError::Silent);
                    };
                    set_slot(&mut param.parameter, Value::Str(s.clone()));
                    self.stmt.bind_text(idx, s.as_bytes())
                };
                if rc == SQLITE_OK {
                    Ok(())
                } else {
                    driver_err(self)
                }
            }
        }
    }

    /// `pdo_sqlite_stmt_col_meta()`: `Err(None)` is a failure without a new
    /// error code (not executed yet).
    pub fn column_meta(&self, colno: i64, executed: bool) -> Result<ZArray, Option<SqlState>> {
        if self.stmt.is_null() || !executed {
            return Err(None);
        }
        if colno >= i64::from(self.stmt.column_count()) {
            return Err(Some(self.h.error()));
        }
        #[allow(clippy::cast_possible_truncation)]
        let col = colno as i32;
        let mut t = HashTable::new();
        let mut flags = HashTable::new();
        let (native, pdo_type): (&str, i64) = match self.stmt.column_type(col) {
            SQLITE_NULL => ("null", PARAM_NULL),
            SQLITE_FLOAT => ("double", PARAM_STR),
            SQLITE_BLOB => {
                let _ = flags.push(Value::from("blob"));
                ("string", PARAM_STR)
            }
            SQLITE_TEXT => ("string", PARAM_STR),
            _ => ("integer", PARAM_INT),
        };
        t.insert(ArrayKey::from_bytes(b"native_type"), Value::from(native));
        t.insert(ArrayKey::from_bytes(b"pdo_type"), Value::Int(pdo_type));
        if let Some(decl) = self.stmt.column_decltype(col) {
            t.insert(
                ArrayKey::from_bytes(b"sqlite:decl_type"),
                Value::Str(ZStr::from(decl)),
            );
        }
        if let Some(table) = self.stmt.column_table_name(col) {
            t.insert(
                ArrayKey::from_bytes(b"table"),
                Value::Str(ZStr::from(table)),
            );
        }
        t.insert(
            ArrayKey::from_bytes(b"flags"),
            Value::Array(ZArray::from_table(flags)),
        );
        Ok(ZArray::from_table(t))
    }

    /// `pdo_sqlite_stmt_cursor_closer()`.
    pub fn cursor_closer(&self) {
        self.stmt.reset();
    }

    /// `pdo_sqlite_stmt_get_attribute()`: `None` when unsupported.
    pub fn get_attribute(&self, attr: i64) -> Option<Value> {
        match attr {
            PDO_SQLITE_ATTR_READONLY_STATEMENT => Some(Value::Bool(self.stmt.readonly())),
            PDO_SQLITE_ATTR_BUSY_STATEMENT => Some(Value::Bool(self.stmt.busy())),
            PDO_SQLITE_ATTR_EXPLAIN_STATEMENT => Some(Value::Int(i64::from(self.stmt.isexplain()))),
            _ => None,
        }
    }

    /// `pdo_sqlite_stmt_set_attribute()`.
    pub fn set_attribute(&self, cx: &mut Cx<'_>, attr: i64, val: &Value) -> bool {
        if attr != PDO_SQLITE_ATTR_EXPLAIN_STATEMENT {
            return false;
        }
        let Value::Int(mode) = val.deref() else {
            cx.throw_new(
                ExceptionClass::TypeError,
                format!(
                    "explain mode must be of type int, {} given",
                    val.value_name()
                ),
                0,
            );
            return false;
        };
        if !(0..=2).contains(&mode) {
            cx.throw_new(
                ExceptionClass::ValueError,
                "explain mode must be one of the Pdo\\Sqlite::EXPLAIN_MODE_* constants",
                0,
            );
            return false;
        }
        #[allow(clippy::cast_possible_truncation)]
        let ok = self.stmt.explain(mode as i32) == SQLITE_OK;
        ok
    }

    /// The prepared SQL (`sqlite3_sql`).
    pub fn sql(&self) -> ZStr {
        ZStr::from(self.stmt.sql())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::{CollectingContext, RuntimeError};

    struct H(CollectingContext);

    impl php_runtime::RuntimeContext for H {
        fn diagnostic(
            &mut self,
            level: php_runtime::ErrorLevel,
            message: &str,
        ) -> php_runtime::RtResult<()> {
            self.0.diagnostic(level, message)
        }
    }

    impl crate::Host for H {
        fn call(&mut self, _: &Value, _: &[Value]) -> Result<Value, RuntimeError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn sqlstate_mapping() {
        assert_eq!(sqlstate_for(0).as_str(), "00000");
        assert_eq!(sqlstate_for(1).as_str(), "HY000");
        assert_eq!(sqlstate_for(12).as_str(), "42S02");
        assert_eq!(sqlstate_for(9).as_str(), "01002");
        assert_eq!(sqlstate_for(22).as_str(), "HYC00");
        assert_eq!(sqlstate_for(18).as_str(), "22001");
        assert_eq!(sqlstate_for(19).as_str(), "23000");
        assert_eq!(sqlstate_for(1555).as_str(), "HY000");
        assert_eq!(
            SqlState(*b"23000").description(),
            Some("Integrity constraint violation")
        );
        assert_eq!(param_type_base(PARAM_STR | 0x4000_0000), PARAM_STR);
        assert_eq!(param_type_base(PARAM_INT | 0x8000_0000), PARAM_INT);
    }

    #[test]
    fn long_params() {
        let mut h = H(CollectingContext::new());
        let mut cx = Cx::new(&mut h);
        assert_eq!(get_long_param(&mut cx, &Value::Int(3)), Some(3));
        assert_eq!(get_long_param(&mut cx, &Value::Bool(true)), Some(1));
        assert_eq!(get_long_param(&mut cx, &Value::from("42")), Some(42));
        assert_eq!(get_long_param(&mut cx, &Value::from("7 apples")), Some(7));
        assert!(!cx.has_exception());
        assert_eq!(get_long_param(&mut cx, &Value::from("1.5")), None);
        assert_eq!(
            cx.take_exception().unwrap().message(),
            Some("Attribute value must be of type int for selected attribute, string given")
        );
        assert_eq!(get_long_param(&mut cx, &Value::Float(1.0)), None);
        assert!(cx.take_exception().is_some());
    }
}

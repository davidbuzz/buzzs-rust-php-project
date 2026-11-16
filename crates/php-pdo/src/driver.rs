// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The driver interface: [`DbhDriver`] mirrors `struct pdo_dbh_methods`,
//! [`StmtDriver`] mirrors `struct pdo_stmt_methods`, and [`DriverEntry`]
//! is `pdo_driver_t` (name + handle factory). The registry of available
//! drivers is [`drivers()`].
//!
//! Optional C method-table entries (`NULL` function pointers) are methods
//! returning `Option`, with `None` meaning "not implemented".
//!
//! Failures are reported as [`DriverError`]: the core stores the SQLSTATE
//! in the `error_code` of the handle the method belongs to and then runs
//! its usual error handling (`PDO_HANDLE_DBH_ERR` / `PDO_HANDLE_STMT_ERR`).
//! Exceptions are left pending in the [`DriverCx`].

use crate::host::DriverCx;
use php_runtime::{Value, ZArray, ZStr};
use php_sqlite::pdo::parser::{DefaultScanner, Scanner};
use php_sqlite::pdo::SqlState;
use std::any::Any;
use std::rc::Rc;

/// Why a driver method failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The driver recorded an error with this SQLSTATE (and native details
    /// for [`DbhDriver::fetch_error`]).
    State(SqlState),
    /// `pdo_raise_impl_error(dbh, stmt, sqlstate, message)`.
    Impl(&'static str, String),
    /// Failure without a new error code (an exception may be pending).
    Silent,
}

/// `struct pdo_column_data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnData {
    /// The column name.
    pub name: ZStr,
    /// `maxlen` (`-1` when unknown).
    pub maxlen: i64,
    /// `precision`.
    pub precision: i64,
}

/// `enum pdo_param_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamEvent {
    /// `PDO_PARAM_EVT_ALLOC`.
    Alloc = 0,
    /// `PDO_PARAM_EVT_FREE`.
    Free = 1,
    /// `PDO_PARAM_EVT_EXEC_PRE`.
    ExecPre = 2,
    /// `PDO_PARAM_EVT_EXEC_POST`.
    ExecPost = 3,
    /// `PDO_PARAM_EVT_FETCH_PRE`.
    FetchPre = 4,
    /// `PDO_PARAM_EVT_FETCH_POST`.
    FetchPost = 5,
    /// `PDO_PARAM_EVT_NORMALIZE`.
    Normalize = 6,
}

impl ParamEvent {
    /// The bit of this event in `dbh->skip_param_evt`.
    pub fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// `struct pdo_bound_param_data`.
#[derive(Debug, Clone)]
pub struct BoundParam {
    /// The variable itself (a [`Value::Reference`] for `bindParam()` /
    /// `bindColumn()`).
    pub parameter: Value,
    /// Optional driver parameters (`$driverOptions`).
    pub driver_params: Option<Value>,
    /// 0-based position; `-1` while only the name is known.
    pub paramno: i64,
    /// The name (with its leading `:` for parameters).
    pub name: Option<ZStr>,
    /// `$maxLength` (a hint for pre-allocation).
    pub max_value_len: i64,
    /// `PDO::PARAM_*`, possibly with flag bits.
    pub param_type: i64,
    /// Parameter (`true`) or bound column (`false`).
    pub is_param: bool,
}

/// What a successful `executer` (or `next_rowset`) reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecuteInfo {
    /// `php_pdo_stmt_set_column_count()`.
    pub column_count: i64,
    /// The new `stmt->row_count`, when the driver sets it.
    pub row_count: Option<i64>,
}

/// The statement a [`DbhDriver::prepare`] creates.
pub struct Prepared {
    /// The driver statement (`stmt->methods` + `stmt->driver_data`).
    pub stmt: Rc<dyn StmtDriver>,
    /// `stmt->supports_placeholders` (`PDO_PLACEHOLDER_*` bits; `0` means
    /// the core emulates prepared statements).
    pub supports_placeholders: u8,
    /// `stmt->named_rewrite_template` (e.g. `":pdo%d"`).
    pub named_rewrite_template: Option<&'static str>,
    /// `stmt->bound_param_map` built when the preparer rewrote named
    /// placeholders for a positional-only driver (`pdo_parse_params()` at
    /// prepare time, e.g. native `MySQL` prepares).
    pub bound_param_map: Option<Vec<(php_sqlite::pdo::parser::ParamMapKey, Vec<u8>)>>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("supports_placeholders", &self.supports_placeholders)
            .field("named_rewrite_template", &self.named_rewrite_template)
            .finish_non_exhaustive()
    }
}

/// `struct pdo_dbh_methods`: a connection of one driver.
pub trait DbhDriver: Any {
    /// Upcast for downcasting to the concrete driver (driver-specific
    /// methods such as `Pdo\Sqlite::createFunction()`).
    fn as_any(&self) -> &dyn Any;

    /// `closer`.
    fn close(&self, cx: &mut DriverCx<'_>);

    /// `preparer`: `options` is the driver options array of
    /// `PDO::prepare()` (`None` for `PDO::query()`).
    fn prepare(
        &self,
        cx: &mut DriverCx<'_>,
        sql: &ZStr,
        options: Option<&ZArray>,
    ) -> Result<Prepared, DriverError>;

    /// `doer`: the number of affected rows.
    fn exec(&self, cx: &mut DriverCx<'_>, sql: &ZStr) -> Result<i64, DriverError>;

    /// `quoter`. `error_mode` is the handle's `PDO::ATTR_ERRMODE`.
    fn quote(
        &self,
        cx: &mut DriverCx<'_>,
        unquoted: &[u8],
        param_type: i64,
        error_mode: i64,
    ) -> Option<Result<ZStr, DriverError>> {
        let _ = (cx, unquoted, param_type, error_mode);
        None
    }

    /// `begin`.
    fn begin(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let _ = cx;
        None
    }

    /// `commit`.
    fn commit(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let _ = cx;
        None
    }

    /// `rollback`.
    fn rollback(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let _ = cx;
        None
    }

    /// `set_attribute`: `Some(Err(DriverError::Silent))` is a plain
    /// `false` (unsupported attribute or invalid value).
    fn set_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
        value: &Value,
    ) -> Option<Result<(), DriverError>> {
        let _ = (cx, attr, value);
        None
    }

    /// `last_id`.
    fn last_insert_id(
        &self,
        cx: &mut DriverCx<'_>,
        name: Option<&ZStr>,
    ) -> Option<Result<ZStr, DriverError>> {
        let _ = (cx, name);
        None
    }

    /// `fetch_err`: the native code and message appended to `errorInfo`
    /// (empty when there is nothing to add; `None` when the driver has no
    /// `fetch_err` method). `stmt` is the statement the error belongs to.
    fn fetch_error(&self, stmt: Option<&dyn StmtDriver>) -> Option<Vec<Value>> {
        let _ = stmt;
        None
    }

    /// `get_attribute`: `Ok(None)` is "unsupported attribute" (`0`).
    fn get_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
    ) -> Option<Result<Option<Value>, DriverError>> {
        let _ = (cx, attr);
        None
    }

    /// `check_liveness` of a cached persistent connection.
    fn check_liveness(&self, cx: &mut DriverCx<'_>) -> bool {
        let _ = cx;
        true
    }

    /// `persistent_shutdown`.
    fn persistent_shutdown(&self, cx: &mut DriverCx<'_>) {
        let _ = cx;
    }

    /// `in_transaction` (`None`: the core tracks `dbh->in_txn`).
    fn in_transaction(&self, cx: &mut DriverCx<'_>) -> Option<bool> {
        let _ = cx;
        None
    }

    /// `scanner`: the SQL scanner used for emulated prepares.
    fn scanner(&self) -> &dyn Scanner {
        &DefaultScanner
    }

    /// `get_driver_methods(dbh, PDO_DBH_DRIVER_METHOD_KIND_DBH)`: the
    /// legacy driver-specific methods callable on a plain `PDO` object
    /// (`sqliteCreateFunction`, ...), with the method that replaces each.
    fn driver_methods(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
}

/// `struct pdo_stmt_methods`: a prepared statement of one driver. The
/// `dtor` entry is `Drop`.
pub trait StmtDriver: Any {
    /// Upcast for downcasting to the concrete driver statement.
    fn as_any(&self) -> &dyn Any;

    /// `executer`. `executed` is `stmt->executed`; `active_query` is
    /// `stmt->active_query_string` (the query with emulated placeholders
    /// substituted, when the core emulates prepares).
    fn execute(
        &self,
        cx: &mut DriverCx<'_>,
        executed: bool,
        active_query: Option<&ZStr>,
    ) -> Result<ExecuteInfo, DriverError>;

    /// `fetcher`: `Ok(false)` when there are no more rows.
    fn fetch(
        &self,
        cx: &mut DriverCx<'_>,
        orientation: i64,
        offset: i64,
    ) -> Result<bool, DriverError>;

    /// `describer`.
    fn describe(&self, cx: &mut DriverCx<'_>, colno: i64) -> Result<ColumnData, DriverError>;

    /// `get_col`: the value of a column of the current row.
    fn get_col(&self, cx: &mut DriverCx<'_>, colno: i64) -> Result<Value, DriverError>;

    /// `param_hook`.
    fn param_hook(
        &self,
        cx: &mut DriverCx<'_>,
        param: &mut BoundParam,
        event: ParamEvent,
        executed: bool,
    ) -> Option<Result<(), DriverError>> {
        let _ = (cx, param, event, executed);
        None
    }

    /// `set_attribute`.
    fn set_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
        value: &Value,
    ) -> Option<Result<(), DriverError>> {
        let _ = (cx, attr, value);
        None
    }

    /// `get_attribute`: `Ok(None)` is "unsupported attribute".
    fn get_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
    ) -> Option<Result<Option<Value>, DriverError>> {
        let _ = (cx, attr);
        None
    }

    /// `get_column_meta`.
    fn column_meta(
        &self,
        cx: &mut DriverCx<'_>,
        colno: i64,
        executed: bool,
    ) -> Option<Result<ZArray, DriverError>> {
        let _ = (cx, colno, executed);
        None
    }

    /// `next_rowset`.
    fn next_rowset(&self, cx: &mut DriverCx<'_>) -> Option<Result<ExecuteInfo, DriverError>> {
        let _ = cx;
        None
    }

    /// `cursor_closer`.
    fn cursor_closer(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let _ = cx;
        None
    }
}

/// The connection parameters handed to a driver factory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectParams {
    /// The data source (the DSN after `driver:`).
    pub data_source: Vec<u8>,
    /// The user name.
    pub username: Option<Vec<u8>>,
    /// The password.
    pub password: Option<Vec<u8>>,
    /// `dbh->is_persistent`.
    pub is_persistent: bool,
    /// `dbh->auto_commit` (`PDO::ATTR_AUTOCOMMIT` of the options, default on).
    pub auto_commit: bool,
    /// `dbh->stringify`.
    pub stringify: bool,
}

/// The per-connection settings a driver factory fills in on `pdo_dbh_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbhFlags {
    /// `alloc_own_columns`: describe columns right after the first execute.
    pub alloc_own_columns: bool,
    /// `max_escaped_char_length`.
    pub max_escaped_char_length: u8,
    /// `skip_param_evt`: bit mask of [`ParamEvent`]s not dispatched.
    pub skip_param_evt: u8,
    /// `native_case` (`PDO::CASE_*`).
    pub native_case: i64,
}

impl Default for DbhFlags {
    fn default() -> Self {
        DbhFlags {
            alloc_own_columns: false,
            max_escaped_char_length: 0,
            skip_param_evt: 0,
            native_case: crate::consts::CASE_NATURAL,
        }
    }
}

/// A successful connection.
pub struct Connection {
    /// The driver data and methods.
    pub driver: Rc<dyn DbhDriver>,
    /// The handle settings.
    pub flags: DbhFlags,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

/// `db_handle_factory`: on failure the factory throws (usually with
/// [`crate::error::throw_exception`]) and returns `None`; the core throws
/// "Constructor failed" when nothing is pending.
pub type HandleFactory =
    fn(&mut crate::host::Ctx<'_>, &ConnectParams, Option<&ZArray>) -> Option<Connection>;

/// `pdo_driver_t` plus its driver-specific `PDO` subclass.
#[derive(Debug, Clone, Copy)]
pub struct DriverEntry {
    /// The driver name (`"sqlite"`).
    pub name: &'static str,
    /// The `PDO` subclass registered with
    /// `php_pdo_register_driver_specific_ce` (`"Pdo\\Sqlite"`).
    pub specific_class: Option<&'static str>,
    /// The handle factory.
    pub factory: HandleFactory,
}

thread_local! {
    static REGISTRY: std::cell::RefCell<Vec<DriverEntry>> =
        std::cell::RefCell::new(vec![crate::mysql::MYSQL_DRIVER, crate::sqlite::SQLITE_DRIVER]);
}

/// The registered drivers, in registration order (`pdo_driver_hash`).
pub fn drivers() -> Vec<DriverEntry> {
    REGISTRY.with(|r| r.borrow().clone())
}

/// `php_pdo_register_driver()`: `false` when a driver with that name is
/// already registered.
pub fn register_driver(entry: DriverEntry) -> bool {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if r.iter().any(|d| d.name == entry.name) {
            return false;
        }
        r.push(entry);
        true
    })
}

/// `php_pdo_unregister_driver()`.
pub fn unregister_driver(name: &str) {
    REGISTRY.with(|r| r.borrow_mut().retain(|d| d.name != name));
}

/// `pdo_find_driver()`.
pub fn find_driver(name: &[u8]) -> Option<DriverEntry> {
    REGISTRY.with(|r| {
        r.borrow()
            .iter()
            .find(|d| d.name.as_bytes() == name)
            .copied()
    })
}

/// The driver-specific `PDO` subclasses (`pdo_driver_specific_ce_hash`):
/// `(driver name, class name)`.
pub fn driver_specific_classes() -> Vec<(&'static str, &'static str)> {
    drivers()
        .iter()
        .filter_map(|d| d.specific_class.map(|c| (d.name, c)))
        .collect()
}

/// `pdo_drivers()` / `PDO::getAvailableDrivers()`.
pub fn available_drivers() -> ZArray {
    ZArray::from_values(drivers().iter().map(|d| Value::from(d.name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry() {
        assert!(find_driver(b"sqlite").is_some());
        assert!(find_driver(b"mysql").is_some());
        assert!(find_driver(b"pgsql").is_none());
        assert_eq!(available_drivers().len(), drivers().len());
        assert_eq!(ParamEvent::ExecPre.bit(), 4);
    }
}

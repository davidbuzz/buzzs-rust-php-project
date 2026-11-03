//! The `sqlite` driver: a thin adapter from the `php-sqlite` PDO driver
//! ([`PdoSqliteDbh`] / [`PdoSqliteStmt`]) to [`DbhDriver`] / [`StmtDriver`].

use crate::consts::{ATTR_CURSOR, CASE_NATURAL, CURSOR_FWDONLY};
use crate::driver::{
    BoundParam, ColumnData, ConnectParams, Connection, DbhDriver, DbhFlags, DriverEntry,
    DriverError, ExecuteInfo, ParamEvent, Prepared, StmtDriver,
};
use crate::host::{Ctx, DriverCx};
use php_runtime::{Value, ZArray, ZStr};
use php_sqlite::pdo::parser::{Scanner, SqliteScanner};
use php_sqlite::pdo::{
    ConnectError, HookError, ParamEvent as SqliteEvent, PdoBoundParam, PdoSqliteDbh, PdoSqliteStmt,
    SqlState,
};
use std::any::Any;
use std::rc::Rc;

/// The `pdo_sqlite` driver entry.
pub const SQLITE_DRIVER: DriverEntry = DriverEntry {
    name: "sqlite",
    specific_class: Some("Pdo\\Sqlite"),
    factory: connect,
};

/// `pdo_sqlite_handle_factory()`.
fn connect(
    cx: &mut Ctx<'_>,
    params: &ConnectParams,
    options: Option<&ZArray>,
) -> Option<Connection> {
    let r = cx.with_driver(|dcx| PdoSqliteDbh::connect(dcx, &params.data_source, options));
    match r {
        Ok(dbh) => Some(Connection {
            driver: Rc::new(SqliteDbh { dbh }),
            flags: DbhFlags {
                alloc_own_columns: PdoSqliteDbh::ALLOC_OWN_COLUMNS,
                max_escaped_char_length: PdoSqliteDbh::MAX_ESCAPED_CHAR_LENGTH,
                skip_param_evt: PdoSqliteDbh::SKIP_PARAM_EVT,
                native_case: CASE_NATURAL,
            },
        }),
        Err(ConnectError::Exception(e)) => {
            let e = crate::host::convert_php_error(cx.host(), e);
            cx.throw(e);
            None
        }
        Err(ConnectError::Driver {
            sqlstate,
            code,
            message,
        }) => {
            crate::error::throw_exception(
                cx,
                i64::from(code),
                &String::from_utf8_lossy(&message),
                sqlstate.as_str(),
            );
            None
        }
    }
}

/// A SQLite connection.
#[derive(Debug)]
pub struct SqliteDbh {
    dbh: PdoSqliteDbh,
}

impl SqliteDbh {
    /// The underlying driver handle (for the `Pdo\Sqlite` methods).
    pub fn handle(&self) -> &PdoSqliteDbh {
        &self.dbh
    }
}

fn state(s: SqlState) -> DriverError {
    DriverError::State(s)
}

/// `pdo_attr_lval(options, option, default)`.
pub(crate) fn attr_lval(
    cx: &mut DriverCx<'_>,
    options: Option<&ZArray>,
    option: i64,
    default: i64,
) -> i64 {
    match options.and_then(|o| o.get_int(option)) {
        Some(v) => {
            let r = v.deref().to_long(cx.host());
            cx.check(r).unwrap_or(0)
        }
        None => default,
    }
}

impl DbhDriver for SqliteDbh {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn close(&self, _cx: &mut DriverCx<'_>) {
        self.dbh.close();
    }

    fn prepare(
        &self,
        cx: &mut DriverCx<'_>,
        sql: &ZStr,
        options: Option<&ZArray>,
    ) -> Result<Prepared, DriverError> {
        let cursor = attr_lval(cx, options, ATTR_CURSOR, CURSOR_FWDONLY);
        let stmt = self
            .dbh
            .prepare(cx, sql.as_bytes(), cursor)
            .map_err(state)?;
        Ok(Prepared {
            stmt: Rc::new(SqliteStmt { stmt }),
            supports_placeholders: PdoSqliteDbh::SUPPORTS_PLACEHOLDERS,
            named_rewrite_template: None,
        })
    }

    fn exec(&self, cx: &mut DriverCx<'_>, sql: &ZStr) -> Result<i64, DriverError> {
        self.dbh.exec(cx, sql.as_bytes()).map_err(state)
    }

    fn quote(
        &self,
        cx: &mut DriverCx<'_>,
        unquoted: &[u8],
        param_type: i64,
        error_mode: i64,
    ) -> Option<Result<ZStr, DriverError>> {
        Some(
            self.dbh
                .quote(cx, unquoted, param_type, error_mode)
                .ok_or(DriverError::Silent),
        )
    }

    fn begin(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        Some(self.dbh.begin(cx).map_err(state))
    }

    fn commit(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        Some(self.dbh.commit(cx).map_err(state))
    }

    fn rollback(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        Some(self.dbh.rollback(cx).map_err(state))
    }

    fn set_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
        value: &Value,
    ) -> Option<Result<(), DriverError>> {
        Some(if self.dbh.set_attribute(cx, attr, value) {
            Ok(())
        } else {
            Err(DriverError::Silent)
        })
    }

    fn last_insert_id(
        &self,
        _cx: &mut DriverCx<'_>,
        _name: Option<&ZStr>,
    ) -> Option<Result<ZStr, DriverError>> {
        Some(Ok(self.dbh.last_insert_id()))
    }

    fn fetch_error(&self, _stmt: Option<&dyn StmtDriver>) -> Option<Vec<Value>> {
        Some(self.dbh.fetch_error())
    }

    fn get_attribute(
        &self,
        _cx: &mut DriverCx<'_>,
        attr: i64,
    ) -> Option<Result<Option<Value>, DriverError>> {
        Some(Ok(self.dbh.get_attribute(attr)))
    }

    fn persistent_shutdown(&self, _cx: &mut DriverCx<'_>) {
        self.dbh.close();
    }

    fn in_transaction(&self, _cx: &mut DriverCx<'_>) -> Option<bool> {
        Some(self.dbh.in_transaction())
    }

    fn scanner(&self) -> &dyn Scanner {
        &SqliteScanner
    }

    fn driver_methods(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("sqliteCreateFunction", "Pdo\\Sqlite::createFunction"),
            ("sqliteCreateAggregate", "Pdo\\Sqlite::createAggregate"),
            ("sqliteCreateCollation", "Pdo\\Sqlite::createCollation"),
        ]
    }
}

/// A prepared SQLite statement.
#[derive(Debug)]
pub struct SqliteStmt {
    stmt: PdoSqliteStmt,
}

impl SqliteStmt {
    /// The underlying driver statement.
    pub fn handle(&self) -> &PdoSqliteStmt {
        &self.stmt
    }
}

fn hook_error(e: HookError) -> DriverError {
    match e {
        HookError::Driver(s) => DriverError::State(s),
        HookError::Impl(state, msg) => DriverError::Impl(state, msg.to_string()),
        HookError::Silent => DriverError::Silent,
    }
}

impl StmtDriver for SqliteStmt {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn execute(
        &self,
        cx: &mut DriverCx<'_>,
        executed: bool,
        _active_query: Option<&ZStr>,
    ) -> Result<ExecuteInfo, DriverError> {
        let info = self.stmt.execute(cx, executed).map_err(state)?;
        Ok(ExecuteInfo {
            column_count: info.column_count,
            row_count: info.row_count,
        })
    }

    fn fetch(&self, cx: &mut DriverCx<'_>, _ori: i64, _offset: i64) -> Result<bool, DriverError> {
        self.stmt.fetch(cx).map_err(state)
    }

    fn describe(&self, _cx: &mut DriverCx<'_>, colno: i64) -> Result<ColumnData, DriverError> {
        let c = self.stmt.describe(colno).map_err(state)?;
        Ok(ColumnData {
            name: c.name,
            maxlen: c.maxlen,
            precision: c.precision,
        })
    }

    fn get_col(&self, _cx: &mut DriverCx<'_>, colno: i64) -> Result<Value, DriverError> {
        self.stmt.get_col(colno).map_err(state)
    }

    fn param_hook(
        &self,
        cx: &mut DriverCx<'_>,
        param: &mut BoundParam,
        event: ParamEvent,
        executed: bool,
    ) -> Option<Result<(), DriverError>> {
        let ev = match event {
            ParamEvent::Alloc => SqliteEvent::Alloc,
            ParamEvent::Free => SqliteEvent::Free,
            ParamEvent::ExecPre => SqliteEvent::ExecPre,
            ParamEvent::ExecPost => SqliteEvent::ExecPost,
            ParamEvent::FetchPre => SqliteEvent::FetchPre,
            ParamEvent::FetchPost => SqliteEvent::FetchPost,
            ParamEvent::Normalize => SqliteEvent::Normalize,
        };
        let mut p = PdoBoundParam {
            parameter: std::mem::replace(&mut param.parameter, Value::Null),
            paramno: param.paramno,
            name: param.name.clone(),
            param_type: param.param_type,
            is_param: param.is_param,
        };
        let r = self.stmt.param_hook(cx, &mut p, ev, executed);
        param.parameter = p.parameter;
        param.paramno = p.paramno;
        Some(r.map_err(hook_error))
    }

    fn set_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
        value: &Value,
    ) -> Option<Result<(), DriverError>> {
        Some(if self.stmt.set_attribute(cx, attr, value) {
            Ok(())
        } else {
            Err(DriverError::Silent)
        })
    }

    fn get_attribute(
        &self,
        _cx: &mut DriverCx<'_>,
        attr: i64,
    ) -> Option<Result<Option<Value>, DriverError>> {
        Some(Ok(self.stmt.get_attribute(attr)))
    }

    fn column_meta(
        &self,
        _cx: &mut DriverCx<'_>,
        colno: i64,
        executed: bool,
    ) -> Option<Result<ZArray, DriverError>> {
        Some(
            self.stmt
                .column_meta(colno, executed)
                .map_err(|e| e.map_or(DriverError::Silent, DriverError::State)),
        )
    }

    fn cursor_closer(&self, _cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        self.stmt.cursor_closer();
        Some(Ok(()))
    }
}

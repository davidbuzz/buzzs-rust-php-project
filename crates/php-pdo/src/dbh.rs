// Derived from php-src ext/pdo/pdo_dbh.c. Similarity metric to original: 15%
//   Copyright © The PHP Group and Contributors.
//   Author: Wez Furlong <wez@php.net>
//           Marcus Boerger <helly@php.net>
//           Sterling Hughes <sterling@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `PDO` class (`pdo_dbh.c`): connection construction (DSN aliases,
//! `uri:` DSNs, persistent connections, driver-specific subclasses),
//! attributes, transactions, `exec`/`prepare`/`query`/`quote` and the
//! error accessors.

use crate::consts::{
    ATTR_AUTOCOMMIT, ATTR_CASE, ATTR_DEFAULT_FETCH_MODE, ATTR_DRIVER_NAME, ATTR_ERRMODE,
    ATTR_ORACLE_NULLS, ATTR_PERSISTENT, ATTR_STATEMENT_CLASS, ATTR_STRINGIFY_FETCHES, CASE_LOWER,
    CASE_NATURAL, CASE_UPPER, ERRMODE_EXCEPTION, ERRMODE_SILENT, ERRMODE_WARNING, FETCH_BOTH,
    FETCH_CLASS, FETCH_DEFAULT, FETCH_INTO, NULL_EMPTY_STRING, NULL_NATURAL, NULL_TO_STRING,
    PARAM_STR,
};
use crate::driver::{
    driver_specific_classes, find_driver, ConnectParams, DbhDriver, DbhFlags, DriverEntry,
    DriverError,
};
use crate::error::{
    code_str, handle_error, raise_impl_error, ErrCode, Func, ERR_NONE, ERR_UNSET, PDO_CONNECT,
    PDO_CONSTRUCT, PDO_EXEC, PDO_PREPARE, PDO_QUERY, PDO_SET_ATTRIBUTE,
};
use crate::host::{Ctx, NullHost, PdoException, PdoHost};
use crate::stmt::{PdoStatement, Stmt};
use php_runtime::numeric::{is_numeric_string, Number};
use php_runtime::{Obj, PhpObject, RtResult, Value, ZArray, ZStr};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// The name of the base statement class.
pub(crate) const PDO_STATEMENT_CLASS: &str = "PDOStatement";

/// `pdo_dbh_t`: the state of a connection, shared by the `PDO` objects
/// using it (several for a persistent connection).
pub(crate) struct Dbh {
    pub driver: RefCell<Option<Rc<dyn DbhDriver>>>,
    /// `dbh->driver`: set once the connection succeeded.
    pub entry: Cell<Option<DriverEntry>>,
    pub flags: Cell<DbhFlags>,
    pub data_source: RefCell<Vec<u8>>,
    pub username: RefCell<Option<Vec<u8>>>,
    pub password: RefCell<Option<Vec<u8>>>,
    pub is_persistent: Cell<bool>,
    pub auto_commit: Cell<bool>,
    pub in_txn: Cell<bool>,
    pub stringify: Cell<bool>,
    pub error_mode: Cell<i64>,
    pub oracle_nulls: Cell<i64>,
    pub desired_case: Cell<i64>,
    pub error_code: Cell<ErrCode>,
    pub default_fetch_type: Cell<i64>,
    pub persistent_id: RefCell<Option<Vec<u8>>>,
    pub refcount: Cell<u32>,
    pub def_stmt_ce: RefCell<ZStr>,
    pub def_stmt_ctor_args: RefCell<Option<ZArray>>,
    /// `query_stmt` / `query_stmt_obj`: the statement of a failed
    /// `PDO::query()`, whose error the handle reports.
    pub query_stmt: RefCell<Option<(Rc<Stmt>, Value)>>,
    /// `cls_methods[PDO_DBH_DRIVER_METHOD_KIND_DBH]`: the driver's method
    /// table, built by the binding on the first lookup of a method `PDO`
    /// itself lacks (`dbh_method_get` → `pdo_hash_methods`) and destroyed
    /// with the connection (`dbh_free`). Type-erased: the method objects
    /// belong to the VM.
    pub cls_methods: RefCell<Option<Rc<dyn Any>>>,
}

impl std::fmt::Debug for Dbh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dbh")
            .field("driver", &self.entry.get().map(|e| e.name))
            .field("error_code", &code_str(self.error_code.get()))
            .field("is_persistent", &self.is_persistent.get())
            .finish_non_exhaustive()
    }
}

impl Dbh {
    fn new() -> Self {
        Dbh {
            driver: RefCell::new(None),
            entry: Cell::new(None),
            flags: Cell::new(DbhFlags::default()),
            data_source: RefCell::new(Vec::new()),
            username: RefCell::new(None),
            password: RefCell::new(None),
            is_persistent: Cell::new(false),
            auto_commit: Cell::new(false),
            in_txn: Cell::new(false),
            stringify: Cell::new(false),
            error_mode: Cell::new(ERRMODE_SILENT),
            oracle_nulls: Cell::new(NULL_NATURAL),
            desired_case: Cell::new(CASE_NATURAL),
            error_code: Cell::new(ERR_UNSET),
            default_fetch_type: Cell::new(FETCH_DEFAULT),
            persistent_id: RefCell::new(None),
            refcount: Cell::new(0),
            def_stmt_ce: RefCell::new(ZStr::from(PDO_STATEMENT_CLASS)),
            def_stmt_ctor_args: RefCell::new(None),
            query_stmt: RefCell::new(None),
            cls_methods: RefCell::new(None),
        }
    }

    /// The driver methods (`dbh->methods`).
    pub fn driver(&self) -> Option<Rc<dyn DbhDriver>> {
        self.driver.borrow().clone()
    }

    /// `PDO_DBH_CLEAR_ERR()`.
    pub fn clear_err(&self) {
        self.error_code.set(ERR_NONE);
        let old = self.query_stmt.borrow_mut().take();
        drop(old);
    }

    /// `PDO_HANDLE_DBH_ERR()`.
    pub fn handle_err(&self, cx: &mut Ctx<'_>) {
        if self.error_code.get() != ERR_NONE {
            handle_error(cx, self, None);
        }
    }

    /// Records a driver failure on the handle.
    pub fn apply(&self, cx: &mut Ctx<'_>, e: DriverError) {
        match e {
            DriverError::State(s) => self.error_code.set(s.0),
            DriverError::Impl(state, msg) => raise_impl_error(cx, self, None, state, Some(&msg)),
            DriverError::Silent => {}
        }
    }

    /// `pdo_is_in_transaction()`.
    pub fn in_transaction(&self, cx: &mut Ctx<'_>) -> bool {
        if let Some(d) = self.driver() {
            if let Some(b) = cx.with_driver(|dcx| d.in_transaction(dcx)) {
                return b;
            }
        }
        self.in_txn.get()
    }

    /// `dbh_free()`: closes the connection unless other users of a
    /// persistent connection remain.
    fn free(&self, cx: &mut Ctx<'_>, free_persistent: bool) {
        let q = self.query_stmt.borrow_mut().take();
        drop(q);
        if self.is_persistent.get() && !free_persistent {
            let n = self.refcount.get().saturating_sub(1);
            self.refcount.set(n);
            if n != 0 {
                return;
            }
        }
        if let Some(d) = self.driver.borrow_mut().take() {
            cx.with_driver(|dcx| d.close(dcx));
        }
        self.def_stmt_ctor_args.borrow_mut().take();
        self.cls_methods.borrow_mut().take();
    }
}

/// The per-object state of a `PDO` instance (`pdo_dbh_object_t`).
struct PdoObject {
    inner: RefCell<Rc<Dbh>>,
    object: RefCell<Option<Weak<dyn PhpObject>>>,
    class_name: RefCell<ZStr>,
}

impl Drop for PdoObject {
    /// `pdo_dbh_free_storage()`.
    fn drop(&mut self) {
        let dbh = self.inner.borrow().clone();
        let mut host = NullHost::default();
        let mut cx = Ctx::new(&mut host);
        if let Some(d) = dbh.driver() {
            let persistent = dbh.is_persistent.get();
            if (!persistent || dbh.refcount.get() <= 2) && dbh.in_transaction(&mut cx) {
                let _ = cx.with_driver(|dcx| d.rollback(dcx));
                dbh.in_txn.set(false);
            }
            if persistent && dbh.refcount.get() <= 2 {
                cx.with_driver(|dcx| d.persistent_shutdown(dcx));
            }
        }
        dbh.free(&mut cx, false);
    }
}

/// A handle to the state of a `PDO` object. The VM creates one per object
/// ([`Pdo::new`] + [`Pdo::attach_object`]) and calls the methods below
/// to implement the class; clones share the state.
#[derive(Clone)]
pub struct Pdo(Rc<PdoObject>);

impl std::fmt::Debug for Pdo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Pdo").field(&*self.dbh()).finish()
    }
}

impl Default for Pdo {
    fn default() -> Self {
        Self::new()
    }
}

/// The arguments of `PDO::__construct()` / `PDO::connect()`.
#[derive(Debug, Clone, Copy)]
pub struct ConnectArgs<'a> {
    /// `$dsn`.
    pub dsn: &'a [u8],
    /// `$username`.
    pub username: Option<&'a [u8]>,
    /// `$password`.
    pub password: Option<&'a [u8]>,
    /// `$options`.
    pub options: Option<&'a ZArray>,
}

impl<'a> ConnectArgs<'a> {
    /// Arguments with only a DSN.
    pub fn dsn(dsn: &'a [u8]) -> Self {
        ConnectArgs {
            dsn,
            username: None,
            password: None,
            options: None,
        }
    }
}

fn truncate_nul(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// `pdo_attr_lval(options, option, default)`.
fn attr_lval(cx: &mut Ctx<'_>, options: Option<&ZArray>, option: i64, default: i64) -> i64 {
    match options.and_then(|o| o.get_int(option)) {
        Some(v) => {
            let r = v.deref().to_long(cx.host());
            cx.check(r).unwrap_or(0)
        }
        None => default,
    }
}

/// `pdo_get_long_param()`.
pub(crate) fn get_long_param(cx: &mut Ctx<'_>, value: &Value) -> Option<i64> {
    match value.deref() {
        Value::Int(i) => return Some(i),
        Value::Bool(b) => return Some(i64::from(b)),
        Value::Str(s) => {
            if let Some(Number::Int(i)) = is_numeric_string(s.as_bytes(), false) {
                return Some(i);
            }
        }
        _ => {}
    }
    cx.throw_type_error(format!(
        "Attribute value must be of type int for selected attribute, {} given",
        value.value_name()
    ));
    None
}

/// `pdo_get_bool_param()`.
pub(crate) fn get_bool_param(cx: &mut Ctx<'_>, value: &Value) -> Option<bool> {
    match value.deref() {
        Value::Bool(b) => Some(b),
        Value::Int(i) => Some(i != 0),
        _ => {
            cx.throw_type_error(format!(
                "Attribute value must be of type bool for selected attribute, {} given",
                value.value_name()
            ));
            None
        }
    }
}

impl Pdo {
    /// `pdo_dbh_new()`: the state of a new, unconnected `PDO` object.
    pub fn new() -> Self {
        Pdo(Rc::new(PdoObject {
            inner: RefCell::new(Rc::new(Dbh::new())),
            object: RefCell::new(None),
            class_name: RefCell::new(ZStr::from("PDO")),
        }))
    }

    /// Associates the handle with its PHP object (used for the class name
    /// in messages and as the statements' `database_object_handle`).
    pub fn attach_object(&self, obj: &Obj) {
        *self.0.object.borrow_mut() = Some(Rc::downgrade(obj.rc()));
        *self.0.class_name.borrow_mut() = obj.class_name();
    }

    /// The PHP object of this handle, if it is still alive.
    pub fn object(&self) -> Option<Value> {
        let w = self.0.object.borrow().clone()?;
        w.upgrade().map(|rc| Value::Object(Obj::from_rc(rc)))
    }

    pub(crate) fn dbh(&self) -> Rc<Dbh> {
        self.0.inner.borrow().clone()
    }

    /// Whether two handles share the same connection state (persistent
    /// connections).
    pub fn same_connection(a: &Pdo, b: &Pdo) -> bool {
        Rc::ptr_eq(&a.dbh(), &b.dbh())
    }

    /// The driver of a constructed handle, for driver-specific methods
    /// (downcast with [`DbhDriver::as_any`], e.g. to
    /// [`crate::sqlite::SqliteDbh`]).
    pub fn driver(&self) -> Option<Rc<dyn DbhDriver>> {
        let dbh = self.dbh();
        dbh.entry.get()?;
        dbh.driver()
    }

    /// The `SQLite` driver handle, for the `Pdo\Sqlite` methods.
    pub fn sqlite(&self) -> Option<php_sqlite::pdo::PdoSqliteDbh> {
        let d = self.driver()?;
        d.as_any()
            .downcast_ref::<crate::sqlite::SqliteDbh>()
            .map(|s| s.handle().clone())
    }

    /// The `MySQL` driver handle, for the `Pdo\Mysql` methods.
    pub fn mysql_warning_count(&self) -> Option<i64> {
        let d = self.driver()?;
        d.as_any()
            .downcast_ref::<crate::mysql::MysqlDbh>()
            .map(crate::mysql::MysqlDbh::warning_count)
    }

    /// The name of the connected driver (`PDO::ATTR_DRIVER_NAME`).
    pub fn driver_name(&self) -> Option<&'static str> {
        self.dbh().entry.get().map(|e| e.name)
    }

    /// The driver's method table
    /// (`dbh->cls_methods[PDO_DBH_DRIVER_METHOD_KIND_DBH]`), if the binding
    /// built one for this connection ([`Pdo::set_driver_methods`]).
    pub fn driver_methods(&self) -> Option<Rc<dyn Any>> {
        self.dbh().cls_methods.borrow().clone()
    }

    /// Stores the driver's method table (`pdo_hash_methods`): the binding
    /// builds it on the first lookup of a method `PDO` itself lacks
    /// (`dbh_method_get`), and it lives as long as the connection, shared
    /// by the objects of a persistent connection and destroyed with it
    /// (`dbh_free`). Type-erased because the method objects belong to the
    /// VM.
    pub fn set_driver_methods(&self, table: Rc<dyn Any>) {
        *self.dbh().cls_methods.borrow_mut() = Some(table);
    }

    /// The values the VM must report to its garbage collector
    /// (`dbh_get_gc`): the default statement constructor arguments.
    pub fn gc_values(&self) -> Vec<Value> {
        self.dbh()
            .def_stmt_ctor_args
            .borrow()
            .clone()
            .map(Value::Array)
            .into_iter()
            .collect()
    }

    /// The current error mode (`PDO::ATTR_ERRMODE`).
    pub fn error_mode(&self) -> i64 {
        self.dbh().error_mode.get()
    }

    /// `PDO_CONSTRUCT_CHECK`.
    fn check(&self, cx: &mut Ctx<'_>) -> Option<Rc<Dbh>> {
        let dbh = self.dbh();
        if dbh.entry.get().is_none() {
            cx.throw_error(format!(
                "{} object is uninitialized",
                self.0.class_name.borrow()
            ));
            return None;
        }
        Some(dbh)
    }

    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    /// `PDO::__construct()`. `called_class` is the class of the object
    /// being constructed.
    pub fn construct(
        &self,
        host: &mut dyn PdoHost,
        called_class: &ZStr,
        args: ConnectArgs<'_>,
    ) -> RtResult<()> {
        let mut cx = Ctx::new(host);
        construct_driver(&mut cx, Some(self), called_class, args);
        cx.finish(())
    }

    /// `PDO::connect()` (static): creates the object of the driver-specific
    /// subclass (or `called_class`) and connects it.
    pub fn connect(
        host: &mut dyn PdoHost,
        called_class: &ZStr,
        args: ConnectArgs<'_>,
    ) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let v = construct_driver(&mut cx, None, called_class, args);
        cx.finish(v.unwrap_or(Value::Null))
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    /// `PDO::prepare()`: the statement object, or `false`.
    pub fn prepare(
        &self,
        host: &mut dyn PdoHost,
        query: &ZStr,
        options: Option<&ZArray>,
    ) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let v = self.prepare_impl(&mut cx, query, options);
        cx.finish(v)
    }

    fn prepare_impl(&self, cx: &mut Ctx<'_>, query: &ZStr, options: Option<&ZArray>) -> Value {
        let Some(dbh) = self.check(cx) else {
            return Value::Null;
        };
        if query.is_empty() {
            cx.throw(PDO_PREPARE.empty_error(1));
            return Value::Null;
        }
        dbh.clear_err();
        let (class, ctor_args) = if let Some(value) =
            options.and_then(|o| o.get_int(ATTR_STATEMENT_CLASS))
        {
            let value = value.clone();
            let Some(arr) = value.as_array() else {
                cx.throw_type_error(format!(
                    "PDO::ATTR_STATEMENT_CLASS value must be of type array, {} given",
                    value.value_name()
                ));
                return Value::Null;
            };
            let Some(item) = arr.get_int(0) else {
                cx.throw_value_error("PDO::ATTR_STATEMENT_CLASS value must be an array with the format array(classname, constructor_args)");
                return Value::Null;
            };
            let info = match item {
                Value::Str(s) => cx.host().lookup_class(s),
                _ => None,
            };
            let Some(info) = info else {
                cx.throw_type_error("PDO::ATTR_STATEMENT_CLASS class must be a valid class");
                return Value::Null;
            };
            if !cx.host().instance_of(&info.name, PDO_STATEMENT_CLASS) {
                cx.throw_type_error(
                    "PDO::ATTR_STATEMENT_CLASS class must be derived from PDOStatement",
                );
                return Value::Null;
            }
            if info.has_constructor && info.constructor_is_public {
                cx.throw_type_error(
                    "User-supplied statement class cannot have a public constructor",
                );
                return Value::Null;
            }
            let ctor_args = match arr.get_int(1) {
                Some(Value::Array(a)) => Some(a.clone()),
                Some(other) => {
                    cx.throw_type_error(format!(
                        "PDO::ATTR_STATEMENT_CLASS constructor_args must be of type ?array, {} given",
                        other.value_name()
                    ));
                    return Value::Null;
                }
                None => None,
            };
            (info.name, ctor_args)
        } else {
            (
                dbh.def_stmt_ce.borrow().clone(),
                dbh.def_stmt_ctor_args.borrow().clone(),
            )
        };
        let Some((obj, st)) = stmt_instantiate(cx, &class, ctor_args.as_ref()) else {
            return Value::Null;
        };
        let stmt = st.0.clone();
        *stmt.query_string.borrow_mut() = Some(query.clone());
        stmt.default_fetch_type.set(dbh.default_fetch_type.get());
        *stmt.dbh.borrow_mut() = Some(dbh.clone());
        *stmt.dbh_object.borrow_mut() = self.object();
        let Some(driver) = dbh.driver() else {
            return Value::Bool(false);
        };
        match cx.with_driver(|dcx| driver.prepare(dcx, query, options)) {
            Ok(p) => {
                stmt.install(p);
                stmt_construct(cx, &stmt, &obj, ctor_args.as_ref());
                obj
            }
            Err(e) => {
                dbh.apply(cx, e);
                dbh.handle_err(cx);
                Value::Bool(false)
            }
        }
    }

    /// `PDO::query()`: `fetch_mode` and `fetch_mode_args` are the optional
    /// fetch-mode arguments (as for `PDOStatement::setFetchMode()`).
    pub fn query(
        &self,
        host: &mut dyn PdoHost,
        query: &ZStr,
        fetch_mode: Option<i64>,
        fetch_mode_args: &[Value],
    ) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let v = self.query_impl(&mut cx, query, fetch_mode, fetch_mode_args);
        cx.finish(v)
    }

    fn query_impl(
        &self,
        cx: &mut Ctx<'_>,
        query: &ZStr,
        fetch_mode: Option<i64>,
        args: &[Value],
    ) -> Value {
        let Some(dbh) = self.check(cx) else {
            return Value::Null;
        };
        if query.is_empty() {
            cx.throw(PDO_QUERY.empty_error(1));
            return Value::Null;
        }
        dbh.clear_err();
        let class = dbh.def_stmt_ce.borrow().clone();
        let ctor_args = dbh.def_stmt_ctor_args.borrow().clone();
        let Some((obj, st)) = stmt_instantiate(cx, &class, ctor_args.as_ref()) else {
            return Value::Null;
        };
        let stmt = st.0.clone();
        *stmt.query_string.borrow_mut() = Some(query.clone());
        *stmt.active_query_string.borrow_mut() = Some((query.clone(), false));
        stmt.default_fetch_type.set(dbh.default_fetch_type.get());
        *stmt.dbh.borrow_mut() = Some(dbh.clone());
        *stmt.dbh_object.borrow_mut() = self.object();
        let Some(driver) = dbh.driver() else {
            return Value::Bool(false);
        };
        match cx.with_driver(|dcx| driver.prepare(dcx, query, None)) {
            Ok(p) => {
                stmt.install(p);
                stmt.clear_err();
                let mode_ok = match fetch_mode {
                    None => true,
                    Some(mode) => {
                        crate::fetch::setup_fetch_mode(cx, &stmt, mode, 2, args, PDO_QUERY)
                    }
                };
                if mode_ok {
                    stmt.clear_err();
                    if let Some(info) = stmt.run_executer(cx) {
                        let _ = info;
                        let mut ret = true;
                        if !stmt.executed.get() {
                            if dbh.flags.get().alloc_own_columns {
                                ret = stmt.describe_columns(cx);
                            }
                            stmt.executed.set(true);
                        }
                        if ret {
                            stmt_construct(cx, &stmt, &obj, ctor_args.as_ref());
                            return obj;
                        }
                    }
                }
                // something broke
                *dbh.query_stmt.borrow_mut() = Some((stmt.clone(), obj));
                stmt.dbh_object.borrow_mut().take();
                stmt.handle_err(cx);
            }
            Err(e) => {
                dbh.apply(cx, e);
                dbh.handle_err(cx);
            }
        }
        Value::Bool(false)
    }

    // ------------------------------------------------------------------
    // Transactions
    // ------------------------------------------------------------------

    /// `PDO::beginTransaction()`.
    pub fn begin_transaction(&self, host: &mut dyn PdoHost) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.txn(&mut cx, Txn::Begin);
        cx.finish(r)
    }

    /// `PDO::commit()`.
    pub fn commit(&self, host: &mut dyn PdoHost) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.txn(&mut cx, Txn::Commit);
        cx.finish(r)
    }

    /// `PDO::rollBack()`.
    pub fn roll_back(&self, host: &mut dyn PdoHost) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.txn(&mut cx, Txn::Rollback);
        cx.finish(r)
    }

    fn txn(&self, cx: &mut Ctx<'_>, op: Txn) -> bool {
        let Some(dbh) = self.check(cx) else {
            return false;
        };
        let in_txn = dbh.in_transaction(cx);
        match op {
            Txn::Begin if in_txn => {
                cx.throw_pdo(PdoException::new(
                    "There is already an active transaction",
                    0,
                ));
                return false;
            }
            Txn::Commit | Txn::Rollback if !in_txn => {
                cx.throw_pdo(PdoException::new("There is no active transaction", 0));
                return false;
            }
            _ => {}
        }
        let Some(driver) = dbh.driver() else {
            return false;
        };
        let r = cx.with_driver(|dcx| match op {
            Txn::Begin => driver.begin(dcx),
            Txn::Commit => driver.commit(dcx),
            Txn::Rollback => driver.rollback(dcx),
        });
        match r {
            None if op == Txn::Begin => {
                cx.throw_pdo(PdoException::new(
                    "This driver doesn't support transactions",
                    0,
                ));
                false
            }
            None => false,
            Some(Ok(())) => {
                dbh.in_txn.set(op == Txn::Begin);
                true
            }
            Some(Err(e)) => {
                dbh.apply(cx, e);
                dbh.handle_err(cx);
                false
            }
        }
    }

    /// `PDO::inTransaction()`.
    pub fn in_transaction(&self, host: &mut dyn PdoHost) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = match self.check(&mut cx) {
            Some(dbh) => dbh.in_transaction(&mut cx),
            None => false,
        };
        cx.finish(r)
    }

    // ------------------------------------------------------------------
    // Attributes
    // ------------------------------------------------------------------

    /// `PDO::setAttribute()`.
    pub fn set_attribute(
        &self,
        host: &mut dyn PdoHost,
        attr: i64,
        value: &Value,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let dbh = self.dbh();
        dbh.clear_err();
        let r = match self.check(&mut cx) {
            Some(dbh) => attribute_set(&mut cx, &dbh, attr, value, 2, PDO_SET_ATTRIBUTE),
            None => false,
        };
        cx.finish(r)
    }

    /// `PDO::getAttribute()`.
    pub fn get_attribute(&self, host: &mut dyn PdoHost, attr: i64) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let dbh = self.dbh();
        dbh.clear_err();
        let r = match self.check(&mut cx) {
            Some(dbh) => get_attribute(&mut cx, &dbh, attr),
            None => Value::Null,
        };
        cx.finish(r)
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    /// `PDO::exec()`: the number of affected rows or `false`.
    pub fn exec(&self, host: &mut dyn PdoHost, statement: &ZStr) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = self.exec_impl(&mut cx, statement);
        cx.finish(r)
    }

    fn exec_impl(&self, cx: &mut Ctx<'_>, statement: &ZStr) -> Value {
        if statement.is_empty() {
            cx.throw(PDO_EXEC.empty_error(1));
            return Value::Null;
        }
        let dbh = self.dbh();
        dbh.clear_err();
        let Some(dbh) = self.check(cx) else {
            return Value::Null;
        };
        let Some(driver) = dbh.driver() else {
            return Value::Bool(false);
        };
        match cx.with_driver(|dcx| driver.exec(dcx, statement)) {
            Ok(n) => Value::Int(n),
            Err(e) => {
                dbh.apply(cx, e);
                dbh.handle_err(cx);
                Value::Bool(false)
            }
        }
    }

    /// `PDO::lastInsertId()`: the id string or `false`.
    pub fn last_insert_id(&self, host: &mut dyn PdoHost, name: Option<&ZStr>) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = self.last_insert_id_impl(&mut cx, name);
        cx.finish(r)
    }

    fn last_insert_id_impl(&self, cx: &mut Ctx<'_>, name: Option<&ZStr>) -> Value {
        let Some(dbh) = self.check(cx) else {
            return Value::Null;
        };
        dbh.clear_err();
        let Some(driver) = dbh.driver() else {
            return Value::Bool(false);
        };
        match cx.with_driver(|dcx| driver.last_insert_id(dcx, name)) {
            None => {
                raise_impl_error(
                    cx,
                    &dbh,
                    None,
                    "IM001",
                    Some("driver does not support lastInsertId()"),
                );
                Value::Bool(false)
            }
            Some(Ok(id)) => Value::Str(id),
            Some(Err(e)) => {
                dbh.apply(cx, e);
                dbh.handle_err(cx);
                Value::Bool(false)
            }
        }
    }

    /// `PDO::quote()`: the quoted string or `false`.
    pub fn quote(&self, host: &mut dyn PdoHost, string: &ZStr, param_type: i64) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = self.quote_impl(&mut cx, string, param_type);
        cx.finish(r)
    }

    fn quote_impl(&self, cx: &mut Ctx<'_>, string: &ZStr, param_type: i64) -> Value {
        let Some(dbh) = self.check(cx) else {
            return Value::Null;
        };
        dbh.clear_err();
        let Some(driver) = dbh.driver() else {
            return Value::Bool(false);
        };
        let mode = dbh.error_mode.get();
        match cx.with_driver(|dcx| driver.quote(dcx, string.as_bytes(), param_type, mode)) {
            None => {
                raise_impl_error(
                    cx,
                    &dbh,
                    None,
                    "IM001",
                    Some("driver does not support quoting"),
                );
                Value::Bool(false)
            }
            Some(Ok(q)) => Value::Str(q),
            Some(Err(e)) => {
                dbh.apply(cx, e);
                dbh.handle_err(cx);
                Value::Bool(false)
            }
        }
    }

    /// The default `$type` of `PDO::quote()`.
    pub const QUOTE_DEFAULT_TYPE: i64 = PARAM_STR;

    // ------------------------------------------------------------------
    // Errors
    // ------------------------------------------------------------------

    /// `PDO::errorCode()`: a SQLSTATE string or `null`.
    pub fn error_code(&self, host: &mut dyn PdoHost) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = match self.check(&mut cx) {
            Some(dbh) => {
                if let Some((stmt, _)) = &*dbh.query_stmt.borrow() {
                    Value::from(code_str(stmt.error_code.get()))
                } else {
                    let code = dbh.error_code.get();
                    if code[0] == 0 {
                        Value::Null
                    } else {
                        Value::from(code_str(code))
                    }
                }
            }
            None => Value::Null,
        };
        cx.finish(r)
    }

    /// `PDO::errorInfo()`: `[SQLSTATE, native code, native message]`.
    pub fn error_info(&self, host: &mut dyn PdoHost) -> RtResult<ZArray> {
        let mut cx = Ctx::new(host);
        let r = match self.check(&mut cx) {
            Some(dbh) => {
                let query_stmt = dbh.query_stmt.borrow().as_ref().map(|(s, _)| s.clone());
                let code = match &query_stmt {
                    Some(s) => s.error_code.get(),
                    None => dbh.error_code.get(),
                };
                let mut out = ZArray::from_values([Value::from(code_str(code))]);
                if code != ERR_NONE {
                    if let Some(d) = dbh.driver() {
                        let sd = query_stmt.as_ref().and_then(|s| s.driver());
                        if let Some(extra) = d.fetch_error(sd.as_deref()) {
                            let t = out.make_mut();
                            for v in extra {
                                let _ = t.push(v);
                            }
                        }
                    }
                }
                fill_error_info(&mut out);
                out
            }
            None => ZArray::new(),
        };
        cx.finish(r)
    }

    /// `PDO::getAvailableDrivers()` / `pdo_drivers()`.
    pub fn get_available_drivers() -> ZArray {
        crate::driver::available_drivers()
    }
}

/// Pads an `errorInfo` array with `null`s to three elements.
pub(crate) fn fill_error_info(out: &mut ZArray) {
    let t = out.make_mut();
    while t.len() < 3 {
        let _ = t.push(Value::Null);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Txn {
    Begin,
    Commit,
    Rollback,
}

/// `pdo_stmt_instantiate()`: creates the statement object (without
/// running its constructor).
fn stmt_instantiate(
    cx: &mut Ctx<'_>,
    class: &ZStr,
    ctor_args: Option<&ZArray>,
) -> Option<(Value, PdoStatement)> {
    if ctor_args.is_some() {
        let has_ctor = cx
            .host()
            .lookup_class(class)
            .is_some_and(|c| c.has_constructor);
        if !has_ctor {
            cx.throw_error("User-supplied statement does not accept constructor arguments");
            return None;
        }
    }
    let r = cx.host().instantiate_statement(class);
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            cx.throw(e);
            None
        }
    }
}

/// `pdo_stmt_construct()`: sets `queryString` and runs the constructor.
fn stmt_construct(cx: &mut Ctx<'_>, stmt: &Stmt, obj: &Value, ctor_args: Option<&ZArray>) {
    let qs = stmt
        .query_string
        .borrow()
        .clone()
        .unwrap_or_else(ZStr::empty);
    let r = cx
        .host()
        .std_write_property(obj, &ZStr::from("queryString"), Value::Str(qs));
    if cx.check(r).is_none() {
        return;
    }
    let r = cx.host().call_constructor(obj, ctor_args);
    cx.check(r);
}

/// `pdo_dbh_attribute_set()`: `false` on failure. `arg_num` is the
/// argument the value came from (for error messages of `func`).
#[allow(clippy::too_many_lines)]
fn attribute_set(
    cx: &mut Ctx<'_>,
    dbh: &Dbh,
    attr: i64,
    value: &Value,
    arg_num: u32,
    func: Func,
) -> bool {
    let value = &value.deref();
    match attr {
        ATTR_ERRMODE => {
            let Some(l) = get_long_param(cx, value) else {
                return false;
            };
            if matches!(l, ERRMODE_SILENT | ERRMODE_WARNING | ERRMODE_EXCEPTION) {
                dbh.error_mode.set(l);
                return true;
            }
            cx.throw(func.value_error(
                arg_num,
                "Error mode must be one of the PDO::ERRMODE_* constants",
            ));
            return false;
        }
        ATTR_CASE => {
            let Some(l) = get_long_param(cx, value) else {
                return false;
            };
            if matches!(l, CASE_NATURAL | CASE_UPPER | CASE_LOWER) {
                dbh.desired_case.set(l);
                return true;
            }
            cx.throw(func.value_error(
                arg_num,
                "Case folding mode must be one of the PDO::CASE_* constants",
            ));
            return false;
        }
        ATTR_ORACLE_NULLS => {
            let Some(l) = get_long_param(cx, value) else {
                return false;
            };
            if matches!(l, NULL_NATURAL | NULL_EMPTY_STRING | NULL_TO_STRING) {
                dbh.oracle_nulls.set(l);
                return true;
            }
            cx.throw(func.value_error(
                arg_num,
                "PDO::ATTR_ORACLE_NULLS must be one of the PDO::NULL_* constants",
            ));
            return false;
        }
        ATTR_DEFAULT_FETCH_MODE => {
            let l = if let Value::Array(a) = value {
                if let Some(Value::Int(m)) = a.get_int(0) {
                    if *m == FETCH_INTO || *m == FETCH_CLASS {
                        cx.throw(func.value_error(
                            arg_num,
                            "PDO::FETCH_INTO and PDO::FETCH_CLASS cannot be set as the default fetch mode",
                        ));
                        return false;
                    }
                }
                i64::from(!a.is_empty())
            } else {
                let Some(l) = get_long_param(cx, value) else {
                    return false;
                };
                l
            };
            if l == FETCH_DEFAULT {
                cx.throw(func.value_error(
                    arg_num,
                    "Fetch mode must be a bitmask of PDO::FETCH_* constants",
                ));
                return false;
            }
            // `default_fetch_type` is a uint16_t.
            dbh.default_fetch_type.set(l & 0xFFFF);
            return true;
        }
        ATTR_STRINGIFY_FETCHES => {
            let Some(b) = get_bool_param(cx, value) else {
                return false;
            };
            dbh.stringify.set(b);
            if let Some(d) = dbh.driver() {
                let _ = cx.with_driver(|dcx| d.set_attribute(dcx, attr, value));
            }
            return true;
        }
        ATTR_STATEMENT_CLASS => {
            if dbh.is_persistent.get() {
                raise_impl_error(
                    cx,
                    dbh,
                    None,
                    "HY000",
                    Some("PDO::ATTR_STATEMENT_CLASS cannot be used with persistent PDO instances"),
                );
                return false;
            }
            let Value::Array(arr) = value else {
                cx.throw(func.type_error(
                    arg_num,
                    &format!(
                        "PDO::ATTR_STATEMENT_CLASS value must be of type array, {} given",
                        value.value_name()
                    ),
                ));
                return false;
            };
            let Some(item) = arr.get_int(0) else {
                cx.throw(func.value_error(
                    arg_num,
                    "PDO::ATTR_STATEMENT_CLASS value must be an array with the format array(classname, constructor_args)",
                ));
                return false;
            };
            let info = match item {
                Value::Str(s) => cx.host().lookup_class(s),
                _ => None,
            };
            let Some(info) = info else {
                cx.throw(func.type_error(
                    arg_num,
                    "PDO::ATTR_STATEMENT_CLASS class must be a valid class",
                ));
                return false;
            };
            if !cx.host().instance_of(&info.name, PDO_STATEMENT_CLASS) {
                cx.throw(func.type_error(
                    arg_num,
                    "PDO::ATTR_STATEMENT_CLASS class must be derived from PDOStatement",
                ));
                return false;
            }
            if info.has_constructor && info.constructor_is_public {
                cx.throw(func.type_error(
                    arg_num,
                    "User-supplied statement class cannot have a public constructor",
                ));
                return false;
            }
            let ctor_args = match arr.get_int(1) {
                None => None,
                Some(Value::Array(a)) => Some(a.clone()),
                Some(other) => {
                    cx.throw(func.type_error(
                        arg_num,
                        &format!(
                            "PDO::ATTR_STATEMENT_CLASS constructor_args must be of type ?array, {} given",
                            other.value_name()
                        ),
                    ));
                    return false;
                }
            };
            *dbh.def_stmt_ce.borrow_mut() = info.name;
            *dbh.def_stmt_ctor_args.borrow_mut() = ctor_args;
            return true;
        }
        _ => {}
    }
    let Some(driver) = dbh.driver() else {
        raise_impl_error(
            cx,
            dbh,
            None,
            "IM001",
            Some("driver does not support setting attributes"),
        );
        return false;
    };
    dbh.clear_err();
    match cx.with_driver(|dcx| driver.set_attribute(dcx, attr, value)) {
        None => {
            raise_impl_error(
                cx,
                dbh,
                None,
                "IM001",
                Some("driver does not support setting attributes"),
            );
            false
        }
        Some(Ok(())) => true,
        Some(Err(e)) => {
            dbh.apply(cx, e);
            dbh.handle_err(cx);
            false
        }
    }
}

/// The generic part of `PDO::getAttribute()` plus the driver delegation.
fn get_attribute(cx: &mut Ctx<'_>, dbh: &Dbh, attr: i64) -> Value {
    match attr {
        ATTR_PERSISTENT => return Value::Bool(dbh.is_persistent.get()),
        ATTR_CASE => return Value::Int(dbh.desired_case.get()),
        ATTR_ORACLE_NULLS => return Value::Int(dbh.oracle_nulls.get()),
        ATTR_ERRMODE => return Value::Int(dbh.error_mode.get()),
        ATTR_DRIVER_NAME => {
            return Value::from(dbh.entry.get().map_or("", |e| e.name));
        }
        ATTR_STATEMENT_CLASS => {
            let mut out = ZArray::from_values([Value::Str(dbh.def_stmt_ce.borrow().clone())]);
            if let Some(args) = dbh.def_stmt_ctor_args.borrow().clone() {
                let _ = out.make_mut().push(Value::Array(args));
            }
            return Value::Array(out);
        }
        ATTR_DEFAULT_FETCH_MODE => return Value::Int(dbh.default_fetch_type.get()),
        ATTR_STRINGIFY_FETCHES => return Value::Bool(dbh.stringify.get()),
        _ => {}
    }
    let Some(driver) = dbh.driver() else {
        raise_impl_error(
            cx,
            dbh,
            None,
            "IM001",
            Some("driver does not support getting attributes"),
        );
        return Value::Bool(false);
    };
    match cx.with_driver(|dcx| driver.get_attribute(dcx, attr)) {
        None => {
            raise_impl_error(
                cx,
                dbh,
                None,
                "IM001",
                Some("driver does not support getting attributes"),
            );
            Value::Bool(false)
        }
        Some(Err(e)) => {
            dbh.apply(cx, e);
            dbh.handle_err(cx);
            Value::Bool(false)
        }
        Some(Ok(None)) => {
            raise_impl_error(
                cx,
                dbh,
                None,
                "IM001",
                Some("driver does not support that attribute"),
            );
            Value::Bool(false)
        }
        Some(Ok(Some(v))) => v,
    }
}

/// `create_driver_specific_pdo_object()`: `Err(())` after throwing; for
/// `PDO::connect()` (`connect == true`) the created object.
fn create_driver_specific_object(
    cx: &mut Ctx<'_>,
    driver: &DriverEntry,
    called_scope: &ZStr,
    connect: bool,
) -> Result<Option<(Value, Pdo)>, ()> {
    let method = if connect { "connect" } else { "__construct" };
    let by_name = driver.specific_class;
    let is_pdo = called_scope.as_bytes().eq_ignore_ascii_case(b"PDO");
    let mut by_called = false;
    if !is_pdo {
        for (_, ce) in driver_specific_classes() {
            if cx.host().instance_of(called_scope, ce) {
                by_called = true;
                break;
            }
        }
    }
    let instantiate = |cx: &mut Ctx<'_>, class: &ZStr| -> Result<Option<(Value, Pdo)>, ()> {
        let r = cx.host().instantiate_pdo(class);
        match r {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                cx.throw(e);
                Err(())
            }
        }
    };
    let wrong_driver = |cx: &mut Ctx<'_>, by_name: &str| {
        cx.throw_pdo(PdoException::new(
            format!(
                "{called_scope}::{method}() cannot be used for connecting to the \"{}\" driver, either call {by_name}::{method}() or PDO::{method}() instead",
                driver.name
            ),
            0,
        ));
    };
    if by_called {
        let Some(by_name) = by_name else {
            cx.throw_pdo(PdoException::new(
                format!(
                    "{called_scope}::{method}() cannot be used for connecting to an unknown driver, call PDO::{method}() instead"
                ),
                0,
            ));
            return Err(());
        };
        if !cx.host().instance_of(called_scope, by_name) {
            wrong_driver(cx, by_name);
            return Err(());
        }
        if connect {
            return instantiate(cx, called_scope);
        }
        return Ok(None);
    }
    if !is_pdo && !connect {
        return Ok(None);
    }
    if let Some(by_name) = by_name {
        if !is_pdo {
            wrong_driver(cx, by_name);
            return Err(());
        }
        if connect {
            return instantiate(cx, &ZStr::from(by_name));
        }
    } else if connect {
        return instantiate(cx, called_scope);
    }
    Ok(None)
}

/// `php_pdo_internal_construct_driver()`: connects `current` (for
/// `__construct`) or a new object (for `connect`, returned).
#[allow(clippy::too_many_lines)]
fn construct_driver(
    cx: &mut Ctx<'_>,
    current: Option<&Pdo>,
    called_scope: &ZStr,
    args: ConnectArgs<'_>,
) -> Option<Value> {
    let func = if current.is_some() {
        PDO_CONSTRUCT
    } else {
        PDO_CONNECT
    };
    let mut data_source = truncate_nul(args.dsn).to_vec();
    let username = args.username.map(|u| truncate_nul(u).to_vec());
    let password = args.password.map(|p| truncate_nul(p).to_vec());
    let options = args.options;

    let mut colon = data_source.iter().position(|&b| b == b':');
    if colon.is_none() {
        // a matching DSN alias in php.ini?
        let mut alt = b"pdo.dsn.".to_vec();
        alt.extend_from_slice(&data_source);
        alt.truncate(511);
        let Some(ini) = cx.host().cfg_get_string(&alt) else {
            cx.throw_pdo(func.pdo_exception(1, "must be a valid data source name"));
            return None;
        };
        data_source = truncate_nul(&ini).to_vec();
        colon = data_source.iter().position(|&b| b == b':');
        if colon.is_none() {
            cx.throw_pdo(PdoException::new(
                format!(
                    "invalid data source name (via INI: {})",
                    String::from_utf8_lossy(&alt)
                ),
                0,
            ));
            return None;
        }
    }
    if data_source.starts_with(b"uri:") {
        cx.error(
            php_runtime::ErrorLevel::Deprecated,
            "Looking up the DSN from a URI is deprecated due to possible security concerns with DSNs coming from remote URIs",
        );
        if cx.has_exception() {
            return None;
        }
        let Some(line) = cx.host().read_uri_line(&data_source[4..], 512) else {
            cx.throw_pdo(func.pdo_exception(1, "must be a valid data source URI"));
            return None;
        };
        data_source = truncate_nul(&line).to_vec();
        colon = data_source.iter().position(|&b| b == b':');
        if colon.is_none() {
            cx.throw_pdo(func.pdo_exception(1, "must be a valid data source name (via URI)"));
            return None;
        }
    }
    let colon = colon.unwrap_or(0);
    let Some(driver) = find_driver(&data_source[..colon]) else {
        cx.throw_pdo(PdoException::new("could not find driver", 0));
        return None;
    };

    let created =
        create_driver_specific_object(cx, &driver, called_scope, current.is_none()).ok()?;
    let (new_obj, pdo) = match (current, created) {
        (Some(p), _) => (None, p.clone()),
        (None, Some((v, p))) => (Some(v), p),
        (None, None) => return None,
    };
    let mut dbh = pdo.dbh();

    // is this supposed to be a persistent connection?
    let mut is_persistent = false;
    let mut call_factory = true;
    let mut hash_key: Option<Vec<u8>> = None;
    if let Some(opts) = options {
        if let Some(v) = opts.get_int(crate::consts::ATTR_PERSISTENT) {
            let v = v.deref();
            let mut key = b"PDO:DBH:DSN=".to_vec();
            key.extend_from_slice(&data_source);
            key.push(b':');
            key.extend_from_slice(username.as_deref().unwrap_or_default());
            key.push(b':');
            key.extend_from_slice(password.as_deref().unwrap_or_default());
            match &v {
                Value::Str(s)
                    if !s.is_empty() && is_numeric_string(s.as_bytes(), false).is_none() =>
                {
                    key.push(b':');
                    key.extend_from_slice(truncate_nul(s.as_bytes()));
                    is_persistent = true;
                }
                _ => {
                    let r = v.to_long(cx.host());
                    is_persistent = cx.check(r).unwrap_or(0) != 0;
                }
            }
            hash_key = Some(key);
        }
        if is_persistent {
            let key = hash_key.clone().unwrap_or_default();
            let mut pdbh = cx.host().persistent_list().get(&key);
            if let Some(p) = &pdbh {
                let alive = match p.driver() {
                    Some(d) => cx.with_driver(|dcx| d.check_liveness(dcx)),
                    None => true,
                };
                if !alive {
                    if p.refcount.get() > 1 {
                        p.refcount.set(p.refcount.get() - 1);
                    }
                    cx.host().persistent_list().remove(&key);
                    pdbh = None;
                }
            }
            if pdbh.is_some() {
                call_factory = false;
            } else {
                let p = Dbh::new();
                p.refcount.set(1);
                p.is_persistent.set(true);
                *p.persistent_id.borrow_mut() = Some(key.clone());
                *p.def_stmt_ce.borrow_mut() = dbh.def_stmt_ce.borrow().clone();
                pdbh = Some(Rc::new(p));
            }
            if let Some(p) = pdbh {
                // switch over to the persistent one
                *pdo.0.inner.borrow_mut() = p.clone();
                p.refcount.set(p.refcount.get() + 1);
                dbh = p;
            }
        }
    }

    if call_factory {
        *dbh.data_source.borrow_mut() = data_source[colon + 1..].to_vec();
        dbh.username.borrow_mut().clone_from(&username);
        dbh.password.borrow_mut().clone_from(&password);
        dbh.default_fetch_type.set(FETCH_BOTH);
    }
    let ac = attr_lval(cx, options, ATTR_AUTOCOMMIT, 1);
    dbh.auto_commit.set(ac != 0);
    let em = attr_lval(cx, options, ATTR_ERRMODE, ERRMODE_EXCEPTION);
    dbh.error_mode.set(em & 0xFF);

    let saved = cx.set_throw_warnings(true);
    let mut ok = !call_factory;
    if call_factory {
        let params = ConnectParams {
            data_source: dbh.data_source.borrow().clone(),
            username,
            password,
            is_persistent,
            auto_commit: dbh.auto_commit.get(),
            stringify: dbh.stringify.get(),
        };
        if let Some(conn) = (driver.factory)(cx, &params, options) {
            *dbh.driver.borrow_mut() = Some(conn.driver);
            dbh.flags.set(conn.flags);
            if is_persistent {
                if let Some(id) = dbh.persistent_id.borrow().clone() {
                    cx.host().persistent_list().insert(id, dbh.clone());
                }
            }
            dbh.entry.set(Some(driver));
            ok = true;
        }
    }
    if ok {
        if let Some(opts) = options {
            for (k, v) in opts.iter() {
                if let php_runtime::ArrayKey::Int(attr) = k {
                    let v = v.deref();
                    attribute_set(cx, &dbh, *attr, &v, 4, func);
                }
            }
        }
        cx.set_throw_warnings(saved);
        return Some(new_obj.unwrap_or(Value::Null));
    }
    if is_persistent {
        dbh.refcount.set(dbh.refcount.get().saturating_sub(1));
    }
    cx.set_throw_warnings(saved);
    if !cx.has_exception() {
        cx.throw_pdo(PdoException::new("Constructor failed", 0));
    }
    None
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `PDOStatement` class (`pdo_stmt.c`, everything except the fetch
//! engine in [`crate::fetch`]): parameter and column binding, `execute`,
//! attributes, column metadata, row sets, `debugDumpParams()` and the
//! error accessors.

use crate::consts::{
    param_type, ATTR_EMULATE_PREPARES, CASE_LOWER, CASE_NATURAL, CASE_UPPER, FETCH_ORI_NEXT,
    PARAM_BOOL, PARAM_INT, PARAM_STR,
};
use crate::dbh::{fill_error_info, Dbh};
use crate::driver::{
    BoundParam, ColumnData, DriverError, ExecuteInfo, ParamEvent, Prepared, StmtDriver,
};
use crate::error::{
    code_str, handle_error, raise_impl_error, ErrCode, Func, ERR_NONE, ERR_UNSET, STMT_BIND_COLUMN,
    STMT_BIND_PARAM, STMT_BIND_VALUE, STMT_GET_COLUMN_META,
};
use crate::fetch::FetchUnion;
use crate::host::{Ctx, PdoHost};
use php_runtime::{ArrayKey, HashTable, Obj, PhpObject, RtResult, Value, ZArray, ZStr};
use php_sqlite::pdo::parser::{
    parse_params, ParamMapKey, ParseContext, ParseError, Parsed, ParserParam, PLACEHOLDER_NONE,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// A key of `bound_params` / `bound_columns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParamKey {
    /// A 0-based position.
    Index(i64),
    /// A name (string key).
    Name(ZStr),
}

/// An ordered `HashTable` of bound parameters or columns.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParamTable {
    entries: Vec<(ParamKey, BoundParam)>,
}

impl ParamTable {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, key: &ParamKey) -> Option<&BoundParam> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, p)| p)
    }

    pub fn get_mut(&mut self, key: &ParamKey) -> Option<&mut BoundParam> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p)
    }

    /// `zend_hash_update`: replaces in place or appends.
    pub fn update(&mut self, key: ParamKey, p: BoundParam) {
        if let Some(slot) = self.get_mut(&key) {
            *slot = p;
        } else {
            self.entries.push((key, p));
        }
    }

    pub fn remove(&mut self, key: &ParamKey) {
        self.entries.retain(|(k, _)| k != key);
    }

    pub fn keys(&self) -> Vec<ParamKey> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(ParamKey, BoundParam)> {
        self.entries.iter()
    }
}

/// `bound_param_map`: placeholder positions/names to rewritten names.
pub(crate) type ParamMap = Vec<(ParamMapKey, Vec<u8>)>;

/// `pdo_stmt_t`.
pub(crate) struct Stmt {
    pub object: RefCell<Option<Weak<dyn PhpObject>>>,
    pub class_name: RefCell<ZStr>,
    pub driver: RefCell<Option<Rc<dyn StmtDriver>>>,
    /// `stmt->dbh` (`None`: uninitialized object).
    pub dbh: RefCell<Option<Rc<Dbh>>>,
    /// `database_object_handle`: keeps the `PDO` object alive.
    pub dbh_object: RefCell<Option<Value>>,
    pub error_code: Cell<ErrCode>,
    pub executed: Cell<bool>,
    pub in_fetch: Cell<bool>,
    pub supports_placeholders: Cell<u8>,
    pub in_param_event: Cell<bool>,
    pub execute_incomplete: Cell<bool>,
    pub bound_params: RefCell<Option<ParamTable>>,
    pub bound_param_map: RefCell<Option<ParamMap>>,
    pub bound_columns: RefCell<Option<ParamTable>>,
    pub columns: RefCell<Option<Vec<ColumnData>>>,
    pub column_count: Cell<i64>,
    pub default_fetch_type: Cell<i64>,
    pub fetch: RefCell<FetchUnion>,
    pub lazy_object: RefCell<Option<Weak<dyn PhpObject>>>,
    pub row_count: Cell<i64>,
    pub query_string: RefCell<Option<ZStr>>,
    /// `active_query_string`, with whether it is a different string than
    /// `query_string` (pointer inequality in C).
    pub active_query_string: RefCell<Option<(ZStr, bool)>>,
    pub named_rewrite_template: Cell<Option<&'static str>>,
}

impl Stmt {
    fn new() -> Self {
        Stmt {
            object: RefCell::new(None),
            class_name: RefCell::new(ZStr::from("PDOStatement")),
            driver: RefCell::new(None),
            dbh: RefCell::new(None),
            dbh_object: RefCell::new(None),
            error_code: Cell::new(ERR_UNSET),
            executed: Cell::new(false),
            in_fetch: Cell::new(false),
            supports_placeholders: Cell::new(0),
            in_param_event: Cell::new(false),
            execute_incomplete: Cell::new(false),
            bound_params: RefCell::new(None),
            bound_param_map: RefCell::new(None),
            bound_columns: RefCell::new(None),
            columns: RefCell::new(None),
            column_count: Cell::new(0),
            default_fetch_type: Cell::new(0),
            fetch: RefCell::new(FetchUnion::None),
            lazy_object: RefCell::new(None),
            row_count: Cell::new(0),
            query_string: RefCell::new(None),
            active_query_string: RefCell::new(None),
            named_rewrite_template: Cell::new(None),
        }
    }

    /// The driver statement.
    pub fn driver(&self) -> Option<Rc<dyn StmtDriver>> {
        self.driver.borrow().clone()
    }

    /// `stmt->dbh`.
    pub fn dbh(&self) -> Option<Rc<Dbh>> {
        self.dbh.borrow().clone()
    }

    /// The statement's own object, if still alive.
    pub fn object(&self) -> Option<Value> {
        let w = self.object.borrow().clone()?;
        w.upgrade().map(|rc| Value::Object(Obj::from_rc(rc)))
    }

    /// Installs what the driver's `preparer` produced.
    pub fn install(&self, p: Prepared) {
        *self.driver.borrow_mut() = Some(p.stmt);
        self.supports_placeholders.set(p.supports_placeholders);
        self.named_rewrite_template.set(p.named_rewrite_template);
        if let Some(map) = p.bound_param_map {
            *self.bound_param_map.borrow_mut() = Some(map);
        }
    }

    /// `PDO_STMT_CLEAR_ERR()`.
    pub fn clear_err(&self) {
        self.error_code.set(ERR_NONE);
    }

    /// `PDO_HANDLE_STMT_ERR()`.
    pub fn handle_err(&self, cx: &mut Ctx<'_>) {
        if self.error_code.get() != ERR_NONE {
            if let Some(dbh) = self.dbh() {
                handle_error(cx, &dbh, Some(self));
            }
        }
    }

    /// `pdo_raise_impl_error(stmt->dbh, stmt, ...)`.
    pub fn raise(&self, cx: &mut Ctx<'_>, sqlstate: &str, supp: &str) {
        if let Some(dbh) = self.dbh() {
            raise_impl_error(cx, &dbh, Some(self), sqlstate, Some(supp));
        }
    }

    /// Records a driver failure on the statement.
    pub fn apply(&self, cx: &mut Ctx<'_>, e: DriverError) {
        match e {
            DriverError::State(s) => self.error_code.set(s.0),
            DriverError::Impl(state, msg) => self.raise(cx, state, &msg),
            DriverError::Silent => {}
        }
    }

    /// Applies an `ExecuteInfo` reported by the driver.
    fn apply_info(&self, info: ExecuteInfo) {
        self.set_column_count(info.column_count);
        if let Some(rc) = info.row_count {
            self.row_count.set(rc);
        }
    }

    /// `stmt->methods->executer(stmt)`.
    pub fn run_executer(&self, cx: &mut Ctx<'_>) -> Option<ExecuteInfo> {
        let d = self.driver()?;
        let executed = self.executed.get();
        let active = self
            .active_query_string
            .borrow()
            .as_ref()
            .map(|(q, _)| q.clone());
        match cx.with_driver(|dcx| d.execute(dcx, executed, active.as_ref())) {
            Ok(info) => {
                self.apply_info(info);
                Some(info)
            }
            Err(e) => {
                self.apply(cx, e);
                None
            }
        }
    }

    /// `stmt->methods->fetcher(stmt, ori, offset)`.
    pub fn run_fetcher(&self, cx: &mut Ctx<'_>, ori: i64, offset: i64) -> bool {
        let Some(d) = self.driver() else {
            return false;
        };
        match cx.with_driver(|dcx| d.fetch(dcx, ori, offset)) {
            Ok(b) => b,
            Err(e) => {
                self.apply(cx, e);
                false
            }
        }
    }

    /// `php_pdo_stmt_set_column_count()`.
    pub fn set_column_count(&self, n: i64) {
        if self.columns.borrow().is_none() {
            self.column_count.set(n);
            return;
        }
        if n == self.column_count.get() {
            return;
        }
        self.reset_columns();
        self.column_count.set(n);
    }

    /// `pdo_stmt_reset_columns()`.
    pub fn reset_columns(&self) {
        self.columns.borrow_mut().take();
        self.column_count.set(0);
    }

    /// The name of column `i` (empty if undescribed).
    pub fn column_name(&self, i: i64) -> ZStr {
        let cols = self.columns.borrow();
        usize::try_from(i)
            .ok()
            .and_then(|i| cols.as_ref()?.get(i).map(|c| c.name.clone()))
            .unwrap_or_else(ZStr::empty)
    }

    /// `pdo_stmt_describe_columns()`.
    pub fn describe_columns(&self, cx: &mut Ctx<'_>) -> bool {
        let Some(dbh) = self.dbh() else {
            return false;
        };
        let Some(d) = self.driver() else {
            return false;
        };
        *self.columns.borrow_mut() = Some(Vec::new());
        let native = dbh.flags.get().native_case;
        let desired = dbh.desired_case.get();
        for col in 0..self.column_count.get() {
            let mut c = match cx.with_driver(|dcx| d.describe(dcx, col)) {
                Ok(c) => c,
                Err(e) => {
                    self.apply(cx, e);
                    return false;
                }
            };
            if native != desired && desired != CASE_NATURAL {
                if desired == CASE_LOWER {
                    c.name = c.name.to_ascii_lowercase();
                } else if desired == CASE_UPPER {
                    c.name = c.name.to_ascii_uppercase();
                }
            }
            if let Some(cols) = self.bound_columns.borrow_mut().as_mut() {
                if let Some(p) = cols.get_mut(&ParamKey::Name(c.name.clone())) {
                    p.paramno = col;
                }
            }
            if let Some(cols) = self.columns.borrow_mut().as_mut() {
                cols.push(c);
            }
        }
        true
    }

    /// `pdo_stmt_disallow_reentrant_param_event()`.
    pub fn disallow_reentrant(&self, cx: &mut Ctx<'_>) -> bool {
        if self.in_param_event.get() {
            cx.throw_error("Cannot modify a PDOStatement while parameter hooks are running");
            return false;
        }
        true
    }

    /// Runs the driver's `param_hook` for one parameter: `None` when the
    /// driver has no hook.
    fn hook(
        &self,
        cx: &mut Ctx<'_>,
        param: &mut BoundParam,
        event: ParamEvent,
    ) -> Option<Result<(), DriverError>> {
        let d = self.driver()?;
        let executed = self.executed.get();
        cx.with_driver(|dcx| d.param_hook(dcx, param, event, executed))
    }

    /// `dispatch_param_event()`.
    pub fn dispatch_param_event(&self, cx: &mut Ctx<'_>, event: ParamEvent) -> bool {
        let Some(dbh) = self.dbh() else {
            return true;
        };
        if dbh.flags.get().skip_param_evt & event.bit() != 0 {
            return true;
        }
        self.in_param_event.set(true);
        let mut ret = true;
        'tables: for table in [&self.bound_params, &self.bound_columns] {
            let keys = match &*table.borrow() {
                Some(t) => t.keys(),
                None => continue,
            };
            for key in keys {
                let Some(mut p) = table.borrow().as_ref().and_then(|t| t.get(&key).cloned()) else {
                    continue;
                };
                let r = self.hook(cx, &mut p, event);
                if let Some(t) = table.borrow_mut().as_mut() {
                    if let Some(slot) = t.get_mut(&key) {
                        *slot = p;
                    }
                }
                match r {
                    None => break 'tables,
                    Some(Ok(())) => {}
                    Some(Err(e)) => {
                        self.apply(cx, e);
                        ret = false;
                        break 'tables;
                    }
                }
            }
        }
        self.in_param_event.set(false);
        ret
    }

    /// `rewrite_name_to_position()`.
    fn rewrite_name_to_position(&self, cx: &mut Ctx<'_>, param: &mut BoundParam) -> bool {
        let map = self.bound_param_map.borrow().clone();
        let Some(map) = map else {
            return true;
        };
        if self.named_rewrite_template.get().is_some() {
            return true;
        }
        let Some(name) = param.name.clone() else {
            if let Some((_, n)) = map
                .iter()
                .find(|(k, _)| *k == ParamMapKey::Index(param.paramno))
            {
                param.name = Some(ZStr::from(n.clone()));
                return true;
            }
            self.raise(cx, "HY093", "parameter was not defined");
            return false;
        };
        for (position, (_, n)) in (0_i64..).zip(map.iter()) {
            if n.as_slice() != name.as_bytes() {
                continue;
            }
            if param.paramno >= 0 {
                self.raise(cx, "IM001", "PDO refuses to handle repeating the same :named parameter for multiple positions with this driver, as it might be unsafe to do so.  Consider using a separate name for each parameter instead");
                return false;
            }
            param.paramno = position;
            return true;
        }
        self.raise(cx, "HY093", "parameter was not defined");
        false
    }

    /// `really_register_bound_param()`.
    #[allow(clippy::too_many_lines)]
    pub fn register_bound_param(
        &self,
        cx: &mut Ctx<'_>,
        mut param: BoundParam,
        is_param: bool,
        func: Func,
    ) -> bool {
        // type juggling of the bound variable
        let current = param.parameter.deref();
        let ty = param_type(param.param_type);
        let converted = if ty == PARAM_STR && param.max_value_len <= 0 && !current.is_null() {
            if matches!(current, Value::Str(_)) {
                None
            } else {
                let r = current.to_zstr(cx.host());
                match cx.check(r) {
                    Some(s) => Some(Value::Str(s)),
                    None => return false,
                }
            }
        } else if ty == PARAM_INT && matches!(current, Value::Bool(_)) {
            Some(Value::Int(i64::from(matches!(current, Value::Bool(true)))))
        } else if ty == PARAM_BOOL && matches!(current, Value::Int(_)) {
            Some(Value::Bool(!matches!(current, Value::Int(0))))
        } else {
            None
        };
        if let Some(v) = converted {
            match &param.parameter {
                Value::Reference(r) => {
                    r.set(v);
                }
                _ => param.parameter = v,
            }
        }
        param.is_param = is_param;

        if !is_param {
            if let Some(name) = &param.name {
                let cols = self.columns.borrow().clone();
                if let Some(cols) = cols {
                    let count = usize::try_from(self.column_count.get()).unwrap_or(0);
                    if let Some(i) = cols.iter().take(count).position(|c| c.name == *name) {
                        param.paramno = i64::try_from(i).unwrap_or(-1);
                    }
                    if param.paramno == -1 {
                        cx.throw(func.value_error(
                            1,
                            &format!(
                                "must refer to a column present in the result set, \"{}\" given",
                                name.to_string_lossy()
                            ),
                        ));
                        return false;
                    }
                }
            }
        }

        if let Some(name) = &param.name {
            if is_param && name.as_bytes().first() != Some(&b':') {
                let mut n = Vec::with_capacity(name.len() + 1);
                n.push(b':');
                n.extend_from_slice(name.as_bytes());
                param.name = Some(ZStr::from(n));
            }
        }

        if is_param && !self.rewrite_name_to_position(cx, &mut param) {
            return false;
        }

        if let Some(Err(e)) = self.hook(cx, &mut param, ParamEvent::Normalize) {
            self.apply(cx, e);
            self.handle_err(cx);
            return false;
        }

        let table = if is_param {
            &self.bound_params
        } else {
            &self.bound_columns
        };
        let key = match &param.name {
            Some(n) => ParamKey::Name(n.clone()),
            None => ParamKey::Index(param.paramno),
        };
        {
            let mut t = table.borrow_mut();
            let t = t.get_or_insert_with(ParamTable::default);
            if param.paramno >= 0 {
                t.remove(&ParamKey::Index(param.paramno));
            }
            t.update(key.clone(), param);
        }

        let Some(mut stored) = table.borrow().as_ref().and_then(|t| t.get(&key).cloned()) else {
            return true;
        };
        let r = self.hook(cx, &mut stored, ParamEvent::Alloc);
        if let Some(Err(e)) = r {
            if let Some(t) = table.borrow_mut().as_mut() {
                t.remove(&key);
            }
            self.apply(cx, e);
            self.handle_err(cx);
            false
        } else {
            if let Some(t) = table.borrow_mut().as_mut() {
                if let Some(slot) = t.get_mut(&key) {
                    *slot = stored;
                }
            }
            true
        }
    }

    /// The emulated-prepare path of `execute()`: `pdo_parse_params()` into
    /// `active_query_string`. `false` when something broke.
    fn emulate(&self, cx: &mut Ctx<'_>, dbh: &Dbh) -> bool {
        self.active_query_string.borrow_mut().take();
        let query = self
            .query_string
            .borrow()
            .clone()
            .unwrap_or_else(ZStr::empty);
        let params: Option<Vec<ParserParam>> = self.bound_params.borrow().as_ref().map(|t| {
            t.iter()
                .map(|(k, p)| ParserParam {
                    key: match k {
                        ParamKey::Index(i) => ParamMapKey::Index(*i),
                        ParamKey::Name(n) => ParamMapKey::Name(n.as_bytes().to_vec()),
                    },
                    value: p.parameter.clone(),
                    param_type: p.param_type,
                })
                .collect()
        });
        let mut map = self.bound_param_map.borrow_mut().take().unwrap_or_default();
        let Some(driver) = dbh.driver() else {
            return false;
        };
        let mode = dbh.error_mode.get();
        let supports = self.supports_placeholders.get();
        let template = self.named_rewrite_template.get();
        let result = cx.with_driver(|dcx| {
            let mut quoter =
                |dcx: &mut crate::host::DriverCx<'_>, bytes: &[u8], ty: i64| match driver
                    .quote(dcx, bytes, ty, mode)
                {
                    None => Some(bytes.to_vec()),
                    Some(Ok(q)) => Some(q.as_bytes().to_vec()),
                    Some(Err(DriverError::State(s))) => {
                        dbh.error_code.set(s.0);
                        None
                    }
                    Some(Err(DriverError::Impl(state, _))) => {
                        dbh.error_code.set(crate::error::to_code(state));
                        None
                    }
                    Some(Err(DriverError::Silent)) => None,
                };
            let mut pctx = ParseContext {
                supports_placeholders: supports,
                named_rewrite_template: template,
                bound_params: params.as_deref(),
                bound_param_map: &mut map,
            };
            parse_params(
                dcx,
                driver.scanner(),
                query.as_bytes(),
                &mut pctx,
                Some(&mut quoter),
            )
        });
        if !map.is_empty() {
            *self.bound_param_map.borrow_mut() = Some(map);
        }
        match result {
            Ok(Parsed::Unchanged) => {
                *self.active_query_string.borrow_mut() = Some((query, false));
                true
            }
            Ok(Parsed::Rewritten(q)) => {
                *self.active_query_string.borrow_mut() = Some((ZStr::from(q), true));
                true
            }
            Err(ParseError::Impl { sqlstate, message }) => {
                raise_impl_error(cx, dbh, Some(self), sqlstate, Some(message));
                false
            }
            Err(ParseError::QuoteFailed) => {
                self.error_code.set(dbh.error_code.get());
                false
            }
            Err(ParseError::Exception) => false,
        }
    }

    /// `PDOStatement::execute()`.
    pub fn execute(&self, cx: &mut Ctx<'_>, dbh: &Dbh, params: Option<&ZArray>) -> bool {
        if !self.disallow_reentrant(cx) {
            return false;
        }
        if self.executed.get() {
            if let Some(d) = self.driver() {
                let _ = cx.with_driver(|dcx| d.cursor_closer(dcx));
            }
        }
        self.execute_incomplete.set(true);
        self.clear_err();
        if let Some(params) = params {
            self.bound_params.borrow_mut().take();
            for (k, v) in params.iter() {
                let (name, paramno) = match k {
                    ArrayKey::Str(s) => (Some(s.clone()), -1),
                    ArrayKey::Int(i) => (None, *i),
                };
                let p = BoundParam {
                    parameter: v.clone(),
                    driver_params: None,
                    paramno,
                    name,
                    max_value_len: 0,
                    param_type: PARAM_STR,
                    is_param: true,
                };
                if !self.register_bound_param(cx, p, true, STMT_BIND_PARAM) {
                    self.bound_params.borrow_mut().take();
                    return false;
                }
            }
        }
        let mut ret = true;
        if self.supports_placeholders.get() == PLACEHOLDER_NONE {
            if !self.emulate(cx, dbh) {
                return false;
            }
        } else if !self.dispatch_param_event(cx, ParamEvent::ExecPre) {
            self.handle_err(cx);
            return false;
        }
        if self.run_executer(cx).is_some() {
            self.execute_incomplete.set(false);
            if !self.executed.get() {
                if dbh.flags.get().alloc_own_columns && self.columns.borrow().is_none() {
                    ret = self.describe_columns(cx);
                }
                self.executed.set(true);
            }
            if ret && !self.dispatch_param_event(cx, ParamEvent::ExecPost) {
                self.handle_err(cx);
                return false;
            }
            return ret;
        }
        self.handle_err(cx);
        false
    }
}

/// A handle to the state of a `PDOStatement` object. The VM creates one per
/// object ([`PdoStatement::new`] + [`PdoStatement::attach_object`]);
/// clones share the state.
#[derive(Clone)]
pub struct PdoStatement(pub(crate) Rc<Stmt>);

impl std::fmt::Debug for PdoStatement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdoStatement")
            .field("query_string", &self.0.query_string.borrow())
            .field("error_code", &code_str(self.0.error_code.get()))
            .finish_non_exhaustive()
    }
}

impl Default for PdoStatement {
    fn default() -> Self {
        Self::new()
    }
}

/// The parameter identifier of `bindParam()` / `bindValue()` /
/// `bindColumn()` (`string|int`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamId {
    /// A name (`":name"` or `"name"`; a column name for `bindColumn()`).
    Name(ZStr),
    /// A 1-based position.
    Position(i64),
}

impl PdoStatement {
    /// `pdo_dbstmt_new()`: the state of a new statement object (not usable
    /// until `PDO::prepare()`/`PDO::query()` initialised it).
    pub fn new() -> Self {
        PdoStatement(Rc::new(Stmt::new()))
    }

    /// Associates the handle with its PHP object.
    pub fn attach_object(&self, obj: &Obj) {
        *self.0.object.borrow_mut() = Some(Rc::downgrade(obj.rc()));
        *self.0.class_name.borrow_mut() = obj.class_name();
    }

    /// The PHP object of this statement, if alive.
    pub fn object(&self) -> Option<Value> {
        self.0.object()
    }

    /// Whether two handles are the same statement.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    /// Whether the statement was created by `PDO` (`stmt->dbh != NULL`).
    pub fn is_initialized(&self) -> bool {
        self.0.dbh.borrow().is_some()
    }

    /// The `queryString` the statement was prepared with.
    pub fn query_string(&self) -> Option<ZStr> {
        self.0.query_string.borrow().clone()
    }

    /// The driver statement (for driver-specific methods).
    pub fn driver(&self) -> Option<Rc<dyn StmtDriver>> {
        self.0.driver()
    }

    /// `PHP_STMT_GET_OBJ`: the handle, or the "object is uninitialized"
    /// error.
    pub(crate) fn get(&self, cx: &mut Ctx<'_>) -> Option<Rc<Dbh>> {
        if let Some(d) = self.0.dbh() {
            Some(d)
        } else {
            cx.throw_error(format!(
                "{} object is uninitialized",
                self.0.class_name.borrow()
            ));
            None
        }
    }

    /// `PDOStatement::execute()`.
    pub fn execute(&self, host: &mut dyn PdoHost, params: Option<&ZArray>) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = match self.get(&mut cx) {
            Some(dbh) => self.0.execute(&mut cx, &dbh, params),
            None => false,
        };
        cx.finish(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn bind(
        &self,
        cx: &mut Ctx<'_>,
        func: Func,
        id: &ParamId,
        var: Value,
        ty: i64,
        max_len: i64,
        driver_params: Option<Value>,
        is_param: bool,
    ) -> bool {
        if self.get(cx).is_none() {
            return false;
        }
        if !self.0.disallow_reentrant(cx) {
            return false;
        }
        let (name, paramno) = match id {
            ParamId::Name(n) => {
                if n.is_empty() {
                    cx.throw(func.empty_error(1));
                    return false;
                }
                (Some(n.clone()), -1)
            }
            ParamId::Position(p) if *p > 0 => (None, p - 1),
            ParamId::Position(_) => {
                cx.throw(func.value_error(1, "must be greater than or equal to 1"));
                return false;
            }
        };
        #[allow(clippy::cast_possible_truncation)]
        let param_type = i64::from(ty as i32);
        let p = BoundParam {
            parameter: var,
            driver_params,
            paramno,
            name,
            max_value_len: max_len,
            param_type,
            is_param,
        };
        self.0.register_bound_param(cx, p, is_param, func)
    }

    /// `PDOStatement::bindParam()`: `var` should be a [`Value::Reference`]
    /// to the bound variable.
    pub fn bind_param(
        &self,
        host: &mut dyn PdoHost,
        param: &ParamId,
        var: Value,
        ty: i64,
        max_length: i64,
        driver_options: Option<Value>,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.bind(
            &mut cx,
            STMT_BIND_PARAM,
            param,
            var,
            ty,
            max_length,
            driver_options,
            true,
        );
        cx.finish(r)
    }

    /// `PDOStatement::bindColumn()`: `var` should be a
    /// [`Value::Reference`] to the variable receiving the column.
    pub fn bind_column(
        &self,
        host: &mut dyn PdoHost,
        column: &ParamId,
        var: Value,
        ty: i64,
        max_length: i64,
        driver_options: Option<Value>,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.bind(
            &mut cx,
            STMT_BIND_COLUMN,
            column,
            var,
            ty,
            max_length,
            driver_options,
            false,
        );
        cx.finish(r)
    }

    /// `PDOStatement::bindValue()`.
    pub fn bind_value(
        &self,
        host: &mut dyn PdoHost,
        param: &ParamId,
        value: &Value,
        ty: i64,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.bind(
            &mut cx,
            STMT_BIND_VALUE,
            param,
            value.deref(),
            ty,
            0,
            None,
            true,
        );
        cx.finish(r)
    }

    /// `PDOStatement::rowCount()`.
    pub fn row_count(&self, host: &mut dyn PdoHost) -> RtResult<i64> {
        let mut cx = Ctx::new(host);
        let r = match self.get(&mut cx) {
            Some(_) => self.0.row_count.get(),
            None => 0,
        };
        cx.finish(r)
    }

    /// `PDOStatement::columnCount()`.
    pub fn column_count(&self, host: &mut dyn PdoHost) -> RtResult<i64> {
        let mut cx = Ctx::new(host);
        let r = match self.get(&mut cx) {
            Some(_) => self.0.column_count.get(),
            None => 0,
        };
        cx.finish(r)
    }

    /// `PDOStatement::errorCode()`.
    pub fn error_code(&self, host: &mut dyn PdoHost) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = match self.get(&mut cx) {
            Some(_) => {
                let code = self.0.error_code.get();
                if code[0] == 0 {
                    Value::Null
                } else {
                    Value::from(code_str(code))
                }
            }
            None => Value::Null,
        };
        cx.finish(r)
    }

    /// `PDOStatement::errorInfo()`.
    pub fn error_info(&self, host: &mut dyn PdoHost) -> RtResult<ZArray> {
        let mut cx = Ctx::new(host);
        let r = match self.get(&mut cx) {
            Some(dbh) => {
                let code = self.0.error_code.get();
                let mut out = ZArray::from_values([Value::from(code_str(code))]);
                if code != ERR_NONE {
                    if let Some(d) = dbh.driver() {
                        let sd = self.0.driver();
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

    /// `PDOStatement::setAttribute()`.
    pub fn set_attribute(
        &self,
        host: &mut dyn PdoHost,
        attr: i64,
        value: &Value,
    ) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.set_attribute_impl(&mut cx, attr, value);
        cx.finish(r)
    }

    fn set_attribute_impl(&self, cx: &mut Ctx<'_>, attr: i64, value: &Value) -> bool {
        if self.get(cx).is_none() {
            return false;
        }
        let Some(d) = self.0.driver() else {
            return false;
        };
        let saved = self.0.error_code.get();
        self.0.clear_err();
        match cx.with_driver(|dcx| d.set_attribute(dcx, attr, value)) {
            None => {
                self.0.error_code.set(saved);
                self.0.raise(
                    cx,
                    "IM001",
                    "This driver doesn't support setting attributes",
                );
                false
            }
            Some(Ok(())) => true,
            Some(Err(e)) => {
                self.0.apply(cx, e);
                self.0.handle_err(cx);
                false
            }
        }
    }

    /// `PDOStatement::getAttribute()`.
    pub fn get_attribute(&self, host: &mut dyn PdoHost, attr: i64) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = self.get_attribute_impl(&mut cx, attr);
        cx.finish(r)
    }

    fn generic_attr(&self, attr: i64) -> Option<Value> {
        if attr == ATTR_EMULATE_PREPARES {
            return Some(Value::Bool(
                self.0.supports_placeholders.get() == PLACEHOLDER_NONE,
            ));
        }
        None
    }

    fn get_attribute_impl(&self, cx: &mut Ctx<'_>, attr: i64) -> Value {
        if self.get(cx).is_none() {
            return Value::Null;
        }
        let Some(d) = self.0.driver() else {
            return Value::Bool(false);
        };
        let saved = self.0.error_code.get();
        self.0.clear_err();
        match cx.with_driver(|dcx| d.get_attribute(dcx, attr)) {
            None => {
                self.0.error_code.set(saved);
                if let Some(v) = self.generic_attr(attr) {
                    return v;
                }
                self.0.raise(
                    cx,
                    "IM001",
                    "This driver doesn't support getting attributes",
                );
                Value::Bool(false)
            }
            Some(Err(e)) => {
                self.0.apply(cx, e);
                self.0.handle_err(cx);
                Value::Bool(false)
            }
            Some(Ok(None)) => {
                if let Some(v) = self.generic_attr(attr) {
                    return v;
                }
                self.0
                    .raise(cx, "IM001", "driver doesn't support getting that attribute");
                Value::Bool(false)
            }
            Some(Ok(Some(v))) => v,
        }
    }

    /// `PDOStatement::getColumnMeta()`: the metadata array or `false`.
    pub fn get_column_meta(&self, host: &mut dyn PdoHost, column: i64) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        let r = self.get_column_meta_impl(&mut cx, column);
        cx.finish(r)
    }

    fn get_column_meta_impl(&self, cx: &mut Ctx<'_>, colno: i64) -> Value {
        if self.get(cx).is_none() {
            return Value::Null;
        }
        if colno < 0 {
            cx.throw(STMT_GET_COLUMN_META.value_error(1, "must be greater than or equal to 0"));
            return Value::Null;
        }
        let Some(d) = self.0.driver() else {
            return Value::Bool(false);
        };
        let saved = self.0.error_code.get();
        self.0.clear_err();
        let executed = self.0.executed.get();
        let meta = match cx.with_driver(|dcx| d.column_meta(dcx, colno, executed)) {
            None => {
                self.0.error_code.set(saved);
                self.0
                    .raise(cx, "IM001", "driver doesn't support meta data");
                return Value::Bool(false);
            }
            Some(Err(e)) => {
                self.0.apply(cx, e);
                self.0.handle_err(cx);
                return Value::Bool(false);
            }
            Some(Ok(m)) => m,
        };
        let has_columns = self.0.columns.borrow().is_some();
        if !has_columns || colno >= self.0.column_count.get() {
            self.0.raise(cx, "07009", "invalid column index");
            return Value::Bool(false);
        }
        let col = {
            let cols = self.0.columns.borrow();
            usize::try_from(colno)
                .ok()
                .and_then(|i| cols.as_ref()?.get(i).cloned())
        };
        let mut meta = meta;
        if let Some(col) = col {
            let t: &mut HashTable = meta.make_mut();
            t.insert(ArrayKey::from_bytes(b"name"), Value::Str(col.name));
            t.insert(ArrayKey::from_bytes(b"len"), Value::Int(col.maxlen));
            t.insert(
                ArrayKey::from_bytes(b"precision"),
                Value::Int(col.precision),
            );
        }
        Value::Array(meta)
    }

    /// `PDOStatement::nextRowset()`.
    pub fn next_rowset(&self, host: &mut dyn PdoHost) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.next_rowset_impl(&mut cx);
        cx.finish(r)
    }

    fn next_rowset_impl(&self, cx: &mut Ctx<'_>) -> bool {
        if self.get(cx).is_none() {
            return false;
        }
        let saved = self.0.error_code.get();
        self.0.clear_err();
        match self.do_next_rowset(cx) {
            None => {
                self.0.error_code.set(saved);
                self.0
                    .raise(cx, "IM001", "driver does not support multiple rowsets");
                false
            }
            Some(true) => true,
            Some(false) => {
                self.0.handle_err(cx);
                false
            }
        }
    }

    /// `pdo_stmt_do_next_rowset()`: `None` when the driver has no
    /// `next_rowset` method (columns are kept then).
    fn do_next_rowset(&self, cx: &mut Ctx<'_>) -> Option<bool> {
        let d = self.0.driver()?;
        let saved_cols = self.0.columns.borrow().clone();
        let saved_count = self.0.column_count.get();
        self.0.reset_columns();
        match cx.with_driver(|dcx| d.next_rowset(dcx)) {
            None => {
                *self.0.columns.borrow_mut() = saved_cols;
                self.0.column_count.set(saved_count);
                None
            }
            Some(Err(e)) => {
                self.0.apply(cx, e);
                self.0.executed.set(false);
                Some(false)
            }
            Some(Ok(info)) => {
                self.0.apply_info(info);
                self.0.describe_columns(cx);
                Some(true)
            }
        }
    }

    /// `PDOStatement::closeCursor()`.
    pub fn close_cursor(&self, host: &mut dyn PdoHost) -> RtResult<bool> {
        let mut cx = Ctx::new(host);
        let r = self.close_cursor_impl(&mut cx);
        cx.finish(r)
    }

    fn close_cursor_impl(&self, cx: &mut Ctx<'_>) -> bool {
        if self.get(cx).is_none() || !self.0.disallow_reentrant(cx) {
            return false;
        }
        let Some(d) = self.0.driver() else {
            return false;
        };
        let saved = self.0.error_code.get();
        self.0.clear_err();
        match cx.with_driver(|dcx| d.cursor_closer(dcx)) {
            None => {
                self.0.error_code.set(saved);
                loop {
                    while self.0.run_fetcher(cx, FETCH_ORI_NEXT, 0) {}
                    if self.do_next_rowset(cx) != Some(true) {
                        break;
                    }
                }
                self.0.executed.set(false);
                true
            }
            Some(Ok(())) => {
                self.0.executed.set(false);
                true
            }
            Some(Err(e)) => {
                self.0.apply(cx, e);
                self.0.handle_err(cx);
                false
            }
        }
    }

    /// `PDOStatement::debugDumpParams()`: writes the dump to `php://output`
    /// and returns `null`.
    pub fn debug_dump_params(&self, host: &mut dyn PdoHost) -> RtResult<Value> {
        let mut cx = Ctx::new(host);
        if self.get(&mut cx).is_none() {
            return cx.finish(Value::Null);
        }
        let out = self.debug_dump_bytes();
        let r = cx.host().write_output(&out);
        cx.check(r);
        cx.finish(Value::Null)
    }

    /// The text `debugDumpParams()` prints.
    pub fn debug_dump_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let qs = self
            .0
            .query_string
            .borrow()
            .clone()
            .unwrap_or_else(ZStr::empty);
        out.extend_from_slice(format!("SQL: [{}] ", qs.len()).as_bytes());
        out.extend_from_slice(qs.as_bytes());
        out.push(b'\n');
        if let Some((active, true)) = &*self.0.active_query_string.borrow() {
            out.extend_from_slice(format!("Sent SQL: [{}] ", active.len()).as_bytes());
            out.extend_from_slice(active.as_bytes());
            out.push(b'\n');
        }
        let params = self.0.bound_params.borrow();
        out.extend_from_slice(
            format!("Params:  {}\n", params.as_ref().map_or(0, ParamTable::len)).as_bytes(),
        );
        if let Some(params) = params.as_ref() {
            for (key, p) in params.iter() {
                match key {
                    ParamKey::Name(n) => {
                        out.extend_from_slice(format!("Key: Name: [{}] ", n.len()).as_bytes());
                        out.extend_from_slice(n.as_bytes());
                        out.push(b'\n');
                    }
                    ParamKey::Index(i) => {
                        #[allow(clippy::cast_sign_loss)]
                        let u = *i as u64;
                        out.extend_from_slice(format!("Key: Position #{u}:\n").as_bytes());
                    }
                }
                let name = p.name.clone().unwrap_or_else(ZStr::empty);
                out.extend_from_slice(
                    format!("paramno={}\nname=[{}] \"", p.paramno, name.len()).as_bytes(),
                );
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(
                    format!(
                        "\"\nis_param={}\nparam_type={}\n",
                        i32::from(p.is_param),
                        p.param_type
                    )
                    .as_bytes(),
                );
            }
        }
        out
    }

    /// The values the VM must report to the garbage collector / keep
    /// alive: bound variables, the fetch-into object, ctor args.
    pub fn gc_values(&self) -> Vec<Value> {
        let mut out = Vec::new();
        if let Some(v) = self.0.dbh_object.borrow().clone() {
            out.push(v);
        }
        for t in [&self.0.bound_params, &self.0.bound_columns] {
            if let Some(t) = t.borrow().as_ref() {
                for (_, p) in t.iter() {
                    out.push(p.parameter.clone());
                    if let Some(d) = &p.driver_params {
                        out.push(d.clone());
                    }
                }
            }
        }
        out.extend(self.0.fetch.borrow().gc_values());
        out
    }

    /// The `write_property` handler: writing `queryString` once it is set
    /// is an error ("Property queryString is read only"); `Ok(false)` means
    /// "use the standard handler".
    pub fn check_property_write(name: &[u8], query_string_initialized: bool) -> RtResult<bool> {
        if name == b"queryString" && query_string_initialized {
            return Err(php_runtime::RuntimeError::Error(
                "Property queryString is read only".into(),
            ));
        }
        Ok(false)
    }

    /// The `unset_property` handler for `queryString`.
    pub fn check_property_unset(name: &[u8]) -> RtResult<()> {
        if name == b"queryString" {
            return Err(php_runtime::RuntimeError::Error(
                "Property queryString is read only".into(),
            ));
        }
        Ok(())
    }
}

//! `mysqli_stmt` (ref: `mysqli_api.c` `mysqli_stmt_*`, `mysqli_prepare`,
//! `mysqli_execute_query`, `mysqli.c` `mysqli_stmt::__construct`).

use std::cell::RefCell;
use std::rc::Rc;

use php_mysqlnd::consts::{
    MYSQL_TYPE_VAR_STRING, STMT_ATTR_CURSOR_TYPE, STMT_ATTR_UPDATE_MAX_LENGTH,
};
use php_mysqlnd::stmt::param_type_from_char;
use php_mysqlnd::{Connection, ErrorInfo, ExecuteError, ParamBind, Statement};
use php_runtime::{Obj, PhpRef, Value, ZArray};
use php_vm::ext::Args;
use php_vm::object::{internal, set_internal};
use php_vm::{Vm, VmResult};

use crate::globals::{self, REPORT_INDEX};
use crate::objects::{
    fetch_link, fetch_stmt, new_object, new_result_object, set_stmt_status, MyStmt, Status,
    StmtObj, StmtRes,
};
use crate::report::{flush_diag, report_conn_error, report_index, report_stmt_error, rt_throw};
use crate::util::{affected, bstr, error_list, long_or_string, opt_str, pos, recv};

/// The error state of a statement as a mysqlnd `ErrorInfo`.
fn stmt_error_info(st: &Statement) -> ErrorInfo {
    let errno = st.errno();
    ErrorInfo {
        errno,
        sqlstate: if errno == 0 {
            String::new()
        } else {
            st.sqlstate()
        },
        error: st.error(),
        list: st.error_list(),
    }
}

/// Reports a saved statement error (`MYSQLI_REPORT_STMT_ERROR` on a copy).
fn report_saved(vm: &mut Vm, e: &ErrorInfo) -> VmResult<()> {
    flush_diag(vm)?;
    if globals::report_mode() & crate::globals::REPORT_ERROR != 0 && e.errno != 0 {
        crate::report::report_error(vm, e.sqlstate_or_null(), e.errno, &e.error)?;
    }
    Ok(())
}

/// `close_stmt_and_copy_errors`: closes the statement, then gives the
/// connection the statement's error and its own affected rows back
/// (closing resets both). Returns the statement's error.
fn close_and_copy(st: &Statement, c: &Connection) -> ErrorInfo {
    let e = stmt_error_info(st);
    let affected = c.affected_rows();
    st.close();
    c.restore_error(e.clone());
    c.set_affected_rows(affected);
    e
}

fn wrap_stmt(
    vm: &mut Vm,
    stmt: Statement,
    query: Option<Vec<u8>>,
    status: Status,
) -> VmResult<Value> {
    let o = new_object(vm, "mysqli_stmt")?;
    set_internal(
        &o,
        StmtObj(Some(StmtRes {
            status,
            st: Rc::new(RefCell::new(MyStmt {
                stmt,
                query,
                result_bind: None,
            })),
        })),
    );
    Ok(Value::Object(o))
}

fn stmt_of(vm: &mut Vm, a: &Args, check: Status) -> VmResult<(Obj, Statement, usize)> {
    let (o, b) = recv(vm, a)?;
    let st = fetch_stmt(vm, &o, check)?;
    let s = st.borrow().stmt.clone();
    Ok((o, s, b))
}

fn index_query(query: &[u8]) -> Option<Vec<u8>> {
    (!query.is_empty() && globals::report_mode() & REPORT_INDEX != 0).then(|| query.to_vec())
}

/// `mysqli_prepare()`
pub fn prepare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let (_, c) = fetch_link(vm, &o, Status::Valid)?;
    let q = a.str(b);
    let st = c.stmt_init();
    if crate::report::live(vm, || st.prepare(q.as_bytes()))?.is_err() {
        // mysql_stmt_close() clears the connection's errors: keep them
        let saved = c.error_info();
        st.close();
        c.restore_error(saved);
        report_conn_error(vm, &c)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    wrap_stmt(vm, st, index_query(q.as_bytes()), Status::Valid)
}

/// `mysqli_stmt_init()`
pub fn stmt_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = recv(vm, a)?;
    let (_, c) = fetch_link(vm, &o, Status::Valid)?;
    let st = c.stmt_init();
    wrap_stmt(vm, st, None, Status::Initialized)
}

/// `mysqli_stmt::__construct()`
pub fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let Value::Object(link) = a.at(0) else {
        return Ok(Value::Null);
    };
    let (_, c) = fetch_link(vm, &link, Status::Valid)?;
    let st = c.stmt_init();
    let cell = Rc::new(RefCell::new(MyStmt {
        stmt: st.clone(),
        query: None,
        result_bind: None,
    }));
    set_internal(
        &this,
        StmtObj(Some(StmtRes {
            status: Status::Initialized,
            st: cell,
        })),
    );
    if let Some(q) = opt_str(a, 1) {
        if crate::report::live(vm, || st.prepare(q.as_bytes()))?.is_err() {
            report_stmt_error(vm, &st)?;
            return Ok(Value::Null);
        }
        flush_diag(vm)?;
        set_stmt_status(&this, Status::Valid);
    } else {
        vm.raise(
            crate::report::E_DEPRECATED,
            "Instantiation of mysqli_stmt without providing the $query parameter is deprecated",
        )?;
    }
    Ok(Value::Null)
}

/// `mysqli_stmt_prepare()`
pub fn stmt_prepare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, st, b) = stmt_of(vm, a, Status::Initialized)?;
    let q = a.str(b);
    if crate::report::live(vm, || st.prepare(q.as_bytes()))?.is_err() {
        report_stmt_error(vm, &st)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    set_stmt_status(&o, Status::Valid);
    Ok(Value::Bool(true))
}

/// `mysqli_stmt_bind_param()`
pub fn bind_param(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, b) = stmt_of(vm, a, Status::Valid)?;
    let types = a.str(b);
    let vars: Vec<Value> = a.values.iter().skip(b + 1).cloned().collect();
    if types.is_empty() {
        return Err(a.value_error(vm, pos(a, 2), "must not be empty"));
    }
    if types.len() != vars.len() {
        return Err(vm.throw(
            "ArgumentCountError",
            "The number of elements in the type definition string must match the number of bind variables",
        ));
    }
    if types.len() != st.param_count() as usize {
        return Err(vm.throw(
            "ArgumentCountError",
            "The number of variables must match the number of parameters in the prepared statement",
        ));
    }
    let mut params = Vec::with_capacity(vars.len());
    for (t, v) in types.as_bytes().iter().zip(vars) {
        let Some(ty) = param_type_from_char(*t) else {
            return Err(a.value_error(
                vm,
                pos(a, 2),
                "must only contain the \"b\", \"d\", \"i\", \"s\" type specifiers",
            ));
        };
        let v = match v {
            Value::Reference(r) => Value::Reference(r),
            other => Value::Reference(PhpRef::new(other)),
        };
        params.push(ParamBind::new(ty, v));
    }
    let ok = st.bind_params(params).is_ok();
    report_stmt_error(vm, &st)?;
    Ok(Value::Bool(ok))
}

/// `mysqli_stmt_bind_result()`
pub fn bind_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let cell = fetch_stmt(vm, &o, Status::Valid)?;
    let st = cell.borrow().stmt.clone();
    let refs: Vec<PhpRef> = a
        .values
        .iter()
        .skip(b)
        .map(|v| match v {
            Value::Reference(r) => r.clone(),
            other => PhpRef::new(other.clone()),
        })
        .collect();
    if refs.len() != st.field_count() as usize {
        return Err(vm.throw(
            "ArgumentCountError",
            "Number of bind variables doesn't match number of fields in prepared statement",
        ));
    }
    let ok = st.bind_result().is_ok();
    if ok {
        cell.borrow_mut().result_bind = Some(refs);
    }
    flush_diag(vm)?;
    Ok(Value::Bool(ok))
}

/// Binds the `execute(array $params)` values (all as strings).
fn bind_list(
    vm: &mut Vm,
    a: &Args,
    st: &Statement,
    arr: &ZArray,
    argpos: usize,
) -> VmResult<Option<bool>> {
    if !arr.is_list() {
        return Err(a.value_error(vm, argpos, "must be a list array"));
    }
    let n = arr.len();
    let pc = st.param_count() as usize;
    if n != pc {
        let msg = format!("must consist of exactly {pc} elements, {n} present");
        return Err(a.value_error(vm, argpos, &msg));
    }
    let params = arr
        .values()
        .map(|v| ParamBind::new(MYSQL_TYPE_VAR_STRING, v.deref()))
        .collect();
    Ok(Some(st.bind_params(params).is_ok()))
}

fn run_execute(vm: &mut Vm, st: &Statement) -> VmResult<bool> {
    let r = st.execute(vm);
    match r {
        Ok(()) => Ok(true),
        Err(ExecuteError::Mysql(_)) => Ok(false),
        Err(ExecuteError::Runtime(e)) => Err(rt_throw(vm, e)),
    }
}

/// `mysqli_stmt_execute()` / `mysqli_execute()`
pub fn execute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let cell = fetch_stmt(vm, &o, Status::Valid)?;
    let (st, query) = {
        let c = cell.borrow();
        (c.stmt.clone(), c.query.clone())
    };
    let mut ret = true;
    if let Value::Array(arr) = a.at(b) {
        if bind_list(vm, a, &st, &arr, pos(a, 2))? == Some(false) {
            report_stmt_error(vm, &st)?;
            ret = false;
        }
    }
    if run_execute(vm, &st)? {
        flush_diag(vm)?;
    } else {
        report_stmt_error(vm, &st)?;
        ret = false;
    }
    if globals::report_mode() & REPORT_INDEX != 0 {
        report_index(vm, query.as_deref().unwrap_or_default(), st.server_status())?;
    }
    Ok(Value::Bool(ret))
}

/// `mysqli_execute_query()`
pub fn execute_query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let (_, c) = fetch_link(vm, &o, Status::Valid)?;
    let q = a.str(b);
    let st = c.stmt_init();
    if crate::report::live(vm, || st.prepare(q.as_bytes()))?.is_err() {
        let e = close_and_copy(&st, &c);
        report_saved(vm, &e)?;
        return Ok(Value::Bool(false));
    }
    let query = index_query(q.as_bytes());
    if let Value::Array(arr) = a.at(b + 1) {
        let r = bind_list(vm, a, &st, &arr, pos(a, 3));
        match r {
            Err(t) => {
                st.close();
                return Err(t);
            }
            Ok(Some(false)) => {
                close_and_copy(&st, &c);
                flush_diag(vm)?;
                return Ok(Value::Bool(false));
            }
            Ok(_) => {}
        }
    }
    if !run_execute(vm, &st)? {
        let status = st.server_status();
        let e = close_and_copy(&st, &c);
        report_saved(vm, &e)?;
        if globals::report_mode() & REPORT_INDEX != 0 {
            report_index(vm, query.as_deref().unwrap_or_default(), status)?;
        }
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    if st.field_count() == 0 {
        close_and_copy(&st, &c);
        return Ok(Value::Bool(true));
    }
    if globals::report_mode() & REPORT_INDEX != 0 {
        report_index(vm, query.as_deref().unwrap_or_default(), st.server_status())?;
    }
    let r = st.get_result();
    let Ok(Some(rs)) = r else {
        let e = close_and_copy(&st, &c);
        report_saved(vm, &e)?;
        return Ok(Value::Bool(false));
    };
    close_and_copy(&st, &c);
    let v = new_result_object(vm, rs)?;
    flush_diag(vm)?;
    Ok(v)
}

/// `mysqli_stmt_fetch()`
pub fn fetch(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = recv(vm, a)?;
    let cell = fetch_stmt(vm, &o, Status::Valid)?;
    let (st, binds) = {
        let c = cell.borrow();
        (c.stmt.clone(), c.result_bind.clone())
    };
    match st.fetch() {
        Ok(Some(row)) => {
            flush_diag(vm)?;
            if let Some(binds) = binds {
                for (r, v) in binds.iter().zip(row) {
                    r.set(v);
                }
            }
            Ok(Value::Bool(true))
        }
        Ok(None) => {
            flush_diag(vm)?;
            Ok(Value::Null)
        }
        Err(_) => {
            report_stmt_error(vm, &st)?;
            Ok(Value::Bool(false))
        }
    }
}

/// `mysqli_stmt_get_result()`
pub fn get_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    if let Ok(Some(rs)) = st.get_result() {
        flush_diag(vm)?;
        new_result_object(vm, rs)
    } else {
        report_stmt_error(vm, &st)?;
        Ok(Value::Bool(false))
    }
}

/// `mysqli_stmt_store_result()`
pub fn store_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    if st.store_result().is_err() {
        report_stmt_error(vm, &st)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    Ok(Value::Bool(true))
}

/// `mysqli_stmt_result_metadata()`
pub fn result_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    if let Some(rs) = st.result_metadata() {
        new_result_object(vm, rs)
    } else {
        report_stmt_error(vm, &st)?;
        Ok(Value::Bool(false))
    }
}

/// `mysqli_stmt_affected_rows()`
pub fn affected_rows(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(affected(st.affected_rows()))
}

/// `mysqli_stmt_insert_id()`
pub fn insert_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(long_or_string(st.insert_id()))
}

/// `mysqli_stmt_num_rows()`
pub fn num_rows(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(long_or_string(st.num_rows()))
}

/// `mysqli_stmt_param_count()`
pub fn param_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(Value::Int(i64::from(st.param_count())))
}

/// `mysqli_stmt_field_count()`
pub fn field_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(Value::Int(i64::from(st.field_count())))
}

/// `mysqli_stmt_errno()`
pub fn errno(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Initialized)?;
    Ok(Value::Int(i64::from(st.errno())))
}

/// `mysqli_stmt_error()`
pub fn error(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Initialized)?;
    Ok(bstr(&st.error()))
}

/// `mysqli_stmt_error_list()`
pub fn stmt_error_list(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Initialized)?;
    Ok(error_list(&st.error_list()))
}

/// `mysqli_stmt_sqlstate()`
pub fn sqlstate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(Value::Str(st.sqlstate().into()))
}

/// `mysqli_stmt_attr_set()`
pub fn attr_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, b) = stmt_of(vm, a, Status::Valid)?;
    let attr = a.int(b);
    let mode = a.int(b + 1);
    match u32::try_from(attr) {
        Ok(STMT_ATTR_UPDATE_MAX_LENGTH) => {
            if mode != 0 && mode != 1 {
                return Err(a.value_error(
                    vm,
                    pos(a, 3),
                    "must be 0 or 1 for attribute MYSQLI_STMT_ATTR_UPDATE_MAX_LENGTH",
                ));
            }
        }
        Ok(STMT_ATTR_CURSOR_TYPE) => {
            if mode != 0 && mode != 1 {
                return Err(a.value_error(
                    vm,
                    pos(a, 3),
                    "must be one of the MYSQLI_CURSOR_TYPE_* constants for attribute MYSQLI_STMT_ATTR_CURSOR_TYPE",
                ));
            }
        }
        _ => {
            return Err(a.value_error(
                vm,
                pos(a, 2),
                "must be either MYSQLI_STMT_ATTR_UPDATE_MAX_LENGTH or MYSQLI_STMT_ATTR_CURSOR_TYPE",
            ));
        }
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let r = st.attr_set(attr as u32, mode as u64);
    if r.is_err() {
        report_stmt_error(vm, &st)?;
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(true))
}

/// `mysqli_stmt_attr_get()`
pub fn attr_get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, b) = stmt_of(vm, a, Status::Valid)?;
    let attr = a.int(b);
    let v = u32::try_from(attr).ok().and_then(|x| st.attr_get(x));
    match v {
        Some(v) => Ok(Value::Int(i64::try_from(v).unwrap_or(i64::MAX))),
        None => Err(a.value_error(
            vm,
            pos(a, 2),
            "must be either MYSQLI_STMT_ATTR_UPDATE_MAX_LENGTH or MYSQLI_STMT_ATTR_CURSOR_TYPE",
        )),
    }
}

/// `mysqli_stmt_close()`
pub fn close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, st, _) = stmt_of(vm, a, Status::Valid)?;
    st.close();
    internal::<StmtObj, _>(&o, |s| s.0 = None);
    flush_diag(vm)?;
    Ok(Value::Bool(true))
}

/// `mysqli_stmt_data_seek()`
pub fn data_seek(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let off = a.int(b);
    if off < 0 {
        return Err(a.value_error(vm, pos(a, 2), "must be greater than or equal to 0"));
    }
    let cell = fetch_stmt(vm, &o, Status::Valid)?;
    let st = cell.borrow().stmt.clone();
    #[allow(clippy::cast_sign_loss)]
    if !st.data_seek(off as u64) {
        let f = if a.this.is_some() {
            "mysqli_stmt::data_seek"
        } else {
            "mysqli_stmt_data_seek"
        };
        return Err(vm.throw(
            "Error",
            &format!("{f}(): No result set associated with the statement"),
        ));
    }
    Ok(Value::Null)
}

/// `mysqli_stmt_free_result()`
pub fn free_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    st.free_result();
    flush_diag(vm)?;
    Ok(Value::Null)
}

/// `mysqli_stmt_reset()`
pub fn reset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    let ok = st.reset().is_ok();
    flush_diag(vm)?;
    Ok(Value::Bool(ok))
}

/// `mysqli_stmt_send_long_data()`
pub fn send_long_data(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, b) = stmt_of(vm, a, Status::Valid)?;
    let n = a.int(b);
    if n < 0 {
        return Err(a.value_error(vm, pos(a, 2), "must be greater than or equal to 0"));
    }
    let data = a.str(b + 1);
    let n = u32::try_from(n).unwrap_or(u32::MAX);
    let ok = st.send_long_data(n, data.as_bytes()).is_ok();
    flush_diag(vm)?;
    Ok(Value::Bool(ok))
}

/// `mysqli_stmt_more_results()`
pub fn more_results(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    Ok(Value::Bool(st.more_results()))
}

/// `mysqli_stmt_next_result()`
pub fn next_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    if let Ok(true) = st.next_result() {
        flush_diag(vm)?;
        Ok(Value::Bool(true))
    } else {
        report_stmt_error(vm, &st)?;
        Ok(Value::Bool(false))
    }
}

/// `mysqli_stmt_get_warnings()`
pub fn get_warnings(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, st, _) = stmt_of(vm, a, Status::Valid)?;
    if st.warning_count() == 0 {
        return Ok(Value::Bool(false));
    }
    let Some(c): Option<Connection> = st.connection() else {
        return Ok(Value::Bool(false));
    };
    crate::link::warnings_object(vm, &c)
}

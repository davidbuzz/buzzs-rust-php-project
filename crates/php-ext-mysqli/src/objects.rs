//! Native state of the mysqli objects (`MYSQLI_RESOURCE` behind
//! `mysqli_object::ptr`) and the `MYSQLI_FETCH_RESOURCE*` helpers that
//! produce the "object is already closed" / "not fully initialized"
//! errors.

use std::cell::RefCell;
use std::rc::Rc;

use php_mysqlnd::{CloseType, Connection, ResultSet, Statement};
use php_runtime::{Obj, PhpRef, Value};
use php_vm::object::{class_of, display_name, internal};
use php_vm::{Vm, VmResult};

use crate::globals;

/// `enum mysqli_status`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// `MYSQLI_STATUS_INITIALIZED`
    Initialized = 1,
    /// `MYSQLI_STATUS_VALID`
    Valid = 2,
}

/// `MY_MYSQL`
#[derive(Debug, Default)]
pub struct MyMysql {
    /// The mysqlnd handle (`NULL` once closed).
    pub mysql: Option<Connection>,
    /// Persistent-list key (`mysqli_host_socketport...`).
    pub hash_key: Option<String>,
    /// Multi statements enabled on the server side.
    pub multi_query: bool,
    /// Whether the handle came from / goes back to the persistent pool.
    pub persistent: bool,
    /// Result mode of an async query (`MYSQLI_STORE_RESULT` / `USE_RESULT`).
    pub async_result_fetch_type: i64,
}

/// `MYSQLI_RESOURCE` of a `mysqli` object.
#[derive(Debug)]
pub struct LinkRes {
    /// Object status.
    pub status: Status,
    /// The connection data (shared so handlers can work without holding the
    /// object's internal borrow).
    pub my: Rc<RefCell<MyMysql>>,
}

/// Internal state of `mysqli` objects (`None` = `intern->ptr == NULL`).
#[derive(Debug, Default)]
pub struct LinkObj(pub Option<LinkRes>);

impl Drop for LinkObj {
    /// `mysqli_link_free_storage`
    fn drop(&mut self) {
        if let Some(res) = self.0.take() {
            let mut my = res.my.borrow_mut();
            if my.mysql.is_some() {
                close_link(&mut my, CloseType::Explicit, res.status);
            }
        }
    }
}

/// `php_mysqli_close`: closes or parks (persistent) the connection.
pub fn close_link(my: &mut MyMysql, close_type: CloseType, status: Status) {
    if status > Status::Initialized {
        globals::with(|g| g.num_links -= 1);
    }
    let Some(conn) = my.mysql.take() else {
        return;
    };
    if my.persistent {
        if let Some(key) = my.hash_key.clone() {
            let rollback = globals::with(|g| g.rollback_on_cached_plink);
            conn.end_psession();
            if rollback && conn.rollback(0, None).is_err() {
                conn.close(close_type);
            } else {
                php_mysqlnd::pool::pool_put(&key, conn);
                globals::with(|g| g.num_inactive_persistent += 1);
            }
            globals::with(|g| g.num_active_persistent -= 1);
        }
        my.persistent = false;
    } else {
        conn.close(close_type);
    }
    my.hash_key = None;
}

/// A prepared statement (`MY_STMT`).
#[derive(Debug)]
pub struct MyStmt {
    /// The mysqlnd statement.
    pub stmt: Statement,
    /// The query (kept for `MYSQLI_REPORT_INDEX` messages).
    pub query: Option<Vec<u8>>,
    /// Variables bound with `bind_result()`.
    pub result_bind: Option<Vec<PhpRef>>,
}

/// `MYSQLI_RESOURCE` of a `mysqli_stmt`.
#[derive(Debug)]
pub struct StmtRes {
    /// Object status.
    pub status: Status,
    /// The statement.
    pub st: Rc<RefCell<MyStmt>>,
}

/// Internal state of `mysqli_stmt` objects (dropping the statement closes
/// it implicitly, like `php_clear_stmt_bind`).
#[derive(Debug, Default)]
pub struct StmtObj(pub Option<StmtRes>);

/// `MYSQLI_RESOURCE` of a `mysqli_result`.
#[derive(Debug)]
pub struct ResultRes {
    /// Object status.
    pub status: Status,
    /// The result set.
    pub rs: Rc<RefCell<ResultSet>>,
}

/// Internal state of `mysqli_result` objects.
#[derive(Debug, Default)]
pub struct ResultObj(pub Option<ResultRes>);

/// One `SHOW WARNINGS` row (`MYSQLI_WARNING`).
#[derive(Debug, Clone)]
pub struct Warning {
    /// `reason`
    pub message: Value,
    /// `sqlstate` (always `HY000`).
    pub sqlstate: &'static str,
    /// `errorno`
    pub errno: i64,
}

/// Internal state of `mysqli_warning` objects: the list and the cursor.
#[derive(Debug, Default)]
pub struct WarningObj(pub Option<(Vec<Warning>, usize)>);

/// The class name of `o` as `%pS` prints it.
#[must_use]
pub fn class_name(o: &Obj) -> String {
    display_name(&class_of(o).name).to_string_lossy()
}

fn closed(vm: &mut Vm, o: &Obj) -> php_vm::Throw {
    let msg = format!("{} object is already closed", class_name(o));
    vm.throw("Error", &msg)
}

fn not_init(vm: &mut Vm, o: &Obj) -> php_vm::Throw {
    let msg = format!("{} object is not fully initialized", class_name(o));
    vm.throw("Error", &msg)
}

/// `MYSQLI_FETCH_RESOURCE_CONN`: the connection data of a `mysqli` object.
pub fn fetch_link(vm: &mut Vm, o: &Obj, check: Status) -> VmResult<(Rc<RefCell<MyMysql>>, Connection)> {
    let r = internal::<LinkObj, _>(o, |l| l.0.as_ref().map(|r| (r.status, r.my.clone()))).flatten();
    let Some((status, my)) = r else {
        return Err(closed(vm, o));
    };
    if status < check {
        return Err(not_init(vm, o));
    }
    let conn = my.borrow().mysql.clone();
    match conn {
        Some(c) => Ok((my, c)),
        None => Err(not_init(vm, o)),
    }
}


/// Sets the status of a `mysqli` object that has a resource.
pub fn set_link_status(o: &Obj, s: Status) {
    internal::<LinkObj, _>(o, |l| {
        if let Some(r) = &mut l.0 {
            r.status = s;
        }
    });
}

/// `MYSQLI_FETCH_RESOURCE_STMT`
pub fn fetch_stmt(vm: &mut Vm, o: &Obj, check: Status) -> VmResult<Rc<RefCell<MyStmt>>> {
    let r = internal::<StmtObj, _>(o, |l| l.0.as_ref().map(|r| (r.status, r.st.clone()))).flatten();
    let Some((status, st)) = r else {
        return Err(closed(vm, o));
    };
    if status < check {
        return Err(not_init(vm, o));
    }
    Ok(st)
}

/// Sets the status of a `mysqli_stmt`.
pub fn set_stmt_status(o: &Obj, s: Status) {
    internal::<StmtObj, _>(o, |l| {
        if let Some(r) = &mut l.0 {
            r.status = s;
        }
    });
}

/// `MYSQLI_FETCH_RESOURCE(result, MYSQL_RES *, ...)`
pub fn fetch_result(vm: &mut Vm, o: &Obj, check: Status) -> VmResult<Rc<RefCell<ResultSet>>> {
    let r = internal::<ResultObj, _>(o, |l| l.0.as_ref().map(|r| (r.status, r.rs.clone()))).flatten();
    let Some((status, rs)) = r else {
        return Err(closed(vm, o));
    };
    if status < check {
        return Err(not_init(vm, o));
    }
    Ok(rs)
}

/// Wraps a result set in a new `mysqli_result` object.
pub fn new_result_object(vm: &mut Vm, rs: ResultSet) -> VmResult<Value> {
    let o = new_object(vm, "mysqli_result")?;
    php_vm::object::set_internal(
        &o,
        ResultObj(Some(ResultRes {
            status: Status::Valid,
            rs: Rc::new(RefCell::new(rs)),
        })),
    );
    Ok(Value::Object(o))
}

/// Instantiates a registered class without calling its constructor.
pub fn new_object(vm: &mut Vm, class: &str) -> VmResult<Obj> {
    let Some(c) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    vm.new_object(&c)
}

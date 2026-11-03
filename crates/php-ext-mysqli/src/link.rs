//! `mysqli` connection functions (ref: `mysqli_nonapi.c`
//! `mysqli_common_connect`, `mysqli_api.c`).

use std::cell::RefCell;
use std::rc::Rc;

use php_mysqlnd::consts::opt::{MYSQL_OPT_LOAD_DATA_LOCAL_DIR, MYSQL_OPT_LOCAL_INFILE};
use php_mysqlnd::consts::{
    CLIENT_MULTI_RESULTS, CLIENT_MULTI_STATEMENTS, OPTION_MULTI_STATEMENTS_OFF,
    OPTION_MULTI_STATEMENTS_ON,
};
use php_mysqlnd::{pool, CloseType, Connection, OptionValue};
use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_vm::ext::Args;
use php_vm::object::{internal, set_internal};
use php_vm::{Vm, VmResult};

use crate::globals::{self, REPORT_INDEX};
use crate::objects::{
    close_link, fetch_link, new_object, new_result_object, LinkObj, LinkRes, MyMysql, Status,
};
use crate::report::{
    flush_diag, report_conn_error, report_index, throw_sql_exception, E_DEPRECATED,
};
use crate::util::{
    affected, bstr, error_list, is_method, long_or_string, opt_int, opt_str, pos, recv,
};

/// `MYSQLI_STORE_RESULT`
pub const STORE_RESULT: i64 = 0;
/// `MYSQLI_USE_RESULT`
pub const USE_RESULT: i64 = 1;
/// `MYSQLI_ASYNC`
pub const ASYNC: i64 = 8;
/// `MYSQLI_STORE_RESULT_COPY_DATA`
pub const STORE_RESULT_COPY_DATA: i64 = 16;

/// Pushes `mysqlnd.*` INI values into the driver.
pub fn sync_mysqlnd_ini(vm: &Vm) {
    for e in php_mysqlnd::ini::ENTRIES {
        let v = vm.ini_string(e.name);
        let _ = php_mysqlnd::ini::set(
            e.name,
            v.as_deref().map(|b| std::str::from_utf8(b).unwrap_or("")),
        );
    }
}

fn ini_opt(vm: &Vm, name: &str) -> Option<Vec<u8>> {
    vm.ini_string(name).filter(|v| !v.is_empty())
}

/// `MYSQLI_DISABLE_MQ`
pub fn disable_mq(my: &Rc<RefCell<MyMysql>>, conn: &Connection) {
    let on = my.borrow().multi_query;
    if on {
        let _ = conn.set_server_option(OPTION_MULTI_STATEMENTS_OFF);
        my.borrow_mut().multi_query = false;
    }
}

/// `php_mysqli_init`
fn mysqli_init_impl(vm: &mut Vm, this: Option<&php_runtime::Obj>) -> VmResult<Value> {
    sync_mysqlnd_ini(vm);
    if let Some(t) = this {
        if internal::<LinkObj, _>(t, |l| l.0.is_some()).unwrap_or(false) {
            return Ok(Value::Null);
        }
    }
    let my = MyMysql {
        mysql: Some(Connection::init_ex(true)),
        ..MyMysql::default()
    };
    let res = LinkRes {
        status: Status::Initialized,
        my: Rc::new(RefCell::new(my)),
    };
    if let Some(t) = this {
        set_internal(t, LinkObj(Some(res)));
        Ok(Value::Null)
    } else {
        let o = new_object(vm, "mysqli")?;
        set_internal(&o, LinkObj(Some(res)));
        Ok(Value::Object(o))
    }
}

/// `mysqli_init()`
pub fn mysqli_init(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    mysqli_init_impl(vm, None)
}

/// `mysqli::init()`
pub fn method_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let t = a.this_obj();
    mysqli_init_impl(vm, Some(&t))
}

/// `mysqli_connect()`
pub fn mysqli_connect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    common_connect(vm, a, false, false)
}

/// `mysqli::__construct()`
pub fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    common_connect(vm, a, false, true)?;
    Ok(Value::Null)
}

/// `mysqli_real_connect()`
pub fn mysqli_real_connect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    common_connect(vm, a, true, false)
}

/// `mysqli_common_connect`
#[allow(clippy::too_many_lines)]
fn common_connect(vm: &mut Vm, a: &Args, is_real_connect: bool, in_ctor: bool) -> VmResult<Value> {
    sync_mysqlnd_ini(vm);
    let object = a.this.clone();
    if in_ctor && !a.passed.iter().any(|p| *p) {
        let o = a.this_obj();
        if internal::<LinkObj, _>(&o, |l| l.0.is_some()).unwrap_or(false) {
            return Err(vm.throw("Error", "Cannot call constructor twice"));
        }
        return mysqli_init_impl(vm, Some(&o));
    }
    let mut self_alloced = false;
    let (object, base, my, flags_in) = if is_real_connect {
        let (o, base) = recv(vm, a)?;
        let (my, _) = fetch_link(vm, &o, Status::Initialized)?;
        let fl = a.at(base + 6);
        let fl = if let Value::Int(i) = fl { i } else { 0 };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let fl = (fl as u32) & !CLIENT_MULTI_STATEMENTS;
        (Some(o), base, my, fl)
    } else {
        if in_ctor {
            let o = a.this_obj();
            if internal::<LinkObj, _>(&o, |l| l.0.is_some()).unwrap_or(false) {
                return Err(vm.throw("Error", "Cannot call constructor twice"));
            }
        }
        let existing = object.as_ref().and_then(|o| {
            internal::<LinkObj, _>(o, |l| l.0.as_ref().map(|r| r.my.clone())).flatten()
        });
        let my = existing.unwrap_or_else(|| {
            self_alloced = true;
            Rc::new(RefCell::new(MyMysql::default()))
        });
        (object, 0, my, 0)
    };
    let flags = flags_in | CLIENT_MULTI_RESULTS;

    let mut host = opt_str(a, base).map(|s| s.as_bytes().to_vec());
    let mut user = opt_str(a, base + 1).map(|s| s.as_bytes().to_vec());
    let mut passwd = opt_str(a, base + 2).map(|s| s.as_bytes().to_vec());
    let dbname = opt_str(a, base + 3).map(|s| s.as_bytes().to_vec());
    let mut port = opt_int(a, base + 4).unwrap_or(0);
    let mut socket = opt_str(a, base + 5).map(|s| s.as_bytes().to_vec());

    if socket.as_ref().is_none_or(Vec::is_empty) {
        socket = ini_opt(vm, "mysqli.default_socket");
    }
    if port == 0 {
        port = vm.ini_long("mysqli.default_port");
        if !(0..=65535).contains(&port) {
            port = 0;
        }
    }
    if passwd.is_none() {
        passwd = ini_opt(vm, "mysqli.default_pw");
    }
    if user.is_none() {
        user = ini_opt(vm, "mysqli.default_user");
    }
    if host.as_ref().is_none_or(Vec::is_empty) {
        host = ini_opt(vm, "mysqli.default_host");
    }

    let resource_status = object
        .as_ref()
        .and_then(|o| internal::<LinkObj, _>(o, |l| l.0.as_ref().map(|r| r.status)).flatten());
    if let Some(st) = resource_status {
        if st > Status::Initialized && my.borrow().mysql.is_some() {
            close_link(&mut my.borrow_mut(), CloseType::Implicit, st);
            if let Some(o) = &object {
                crate::objects::set_link_status(o, Status::Initialized);
            }
        }
    }

    let mut persistent = false;
    let mut new_connection = false;
    let mut host_s = host.clone().unwrap_or_default();
    if host_s.len() > 2 && host_s[..2].eq_ignore_ascii_case(b"p:") {
        host_s.drain(..2);
        host = Some(host_s.clone());
        if globals::with(|g| g.allow_persistent) {
            persistent = true;
            let s = |v: &Option<Vec<u8>>| {
                String::from_utf8_lossy(v.as_deref().unwrap_or_default()).into_owned()
            };
            let key = format!(
                "mysqli_{}_{}{}{}{}{}",
                String::from_utf8_lossy(&host_s),
                s(&socket),
                port,
                s(&user),
                s(&dbname),
                s(&passwd)
            );
            {
                let mut m = my.borrow_mut();
                m.persistent = true;
                m.hash_key = Some(key.clone());
            }
            if pool::pool_len(&key) > 0 {
                let old = my.borrow_mut().mysql.take();
                if let Some(old) = old {
                    old.close(CloseType::Implicit);
                }
                if let Some(c) = pool::pool_take(&key) {
                    globals::with(|g| g.num_inactive_persistent -= 1);
                    let ok = c
                        .change_user(
                            user.as_deref().unwrap_or_default(),
                            passwd.as_deref().unwrap_or_default(),
                            dbname.as_deref(),
                            true,
                        )
                        .is_ok();
                    flush_diag(vm)?;
                    if ok {
                        globals::with(|g| g.num_active_persistent += 1);
                        globals::set_connect_error(c.errno(), &c.error());
                        my.borrow_mut().mysql = Some(c);
                        return connect_end(vm, object, my, persistent, false, is_real_connect);
                    }
                    c.close(CloseType::Implicit);
                }
            }
        } else {
            vm.php_warning("Persistent connections are disabled. Downgrading to normal")?;
        }
    }
    let (max_links, num_links) = globals::with(|g| (g.max_links, g.num_links));
    if max_links != -1 && num_links >= max_links {
        vm.php_warning(&format!("Too many open links ({num_links})"))?;
        return connect_err(&my, self_alloced);
    }
    let (max_p, cur_p) = globals::with(|g| {
        (
            g.max_persistent,
            g.num_active_persistent + g.num_inactive_persistent,
        )
    });
    if persistent && max_p != -1 && cur_p >= max_p {
        vm.php_warning(&format!("Too many open persistent links ({cur_p})"))?;
        return connect_err(&my, self_alloced);
    }
    let conn = {
        let mut m = my.borrow_mut();
        if m.mysql.is_none() {
            m.mysql = Some(Connection::init_ex(persistent));
            new_connection = true;
        }
        m.mysql.clone()
    };
    let Some(conn) = conn else {
        return connect_err(&my, self_alloced);
    };
    let host_str = host
        .as_deref()
        .map(|h| String::from_utf8_lossy(h).into_owned());
    let socket_str = socket
        .as_deref()
        .map(|h| String::from_utf8_lossy(h).into_owned());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let r = conn.connect(
        host_str.as_deref(),
        user.as_deref(),
        passwd.as_deref(),
        dbname.as_deref(),
        port as u32,
        socket_str.as_deref(),
        flags,
    );
    if r.is_err() {
        let (errno, err, st) = (conn.errno(), conn.error(), conn.sqlstate());
        globals::set_connect_error(errno, &err);
        if !is_real_connect {
            conn.close(CloseType::Disconnected);
            my.borrow_mut().mysql = None;
        }
        let res = connect_err(&my, self_alloced);
        flush_diag(vm)?;
        throw_sql_exception(vm, &st, i64::from(errno), &err)?;
        return res;
    }
    flush_diag(vm)?;
    globals::set_connect_error(conn.errno(), &conn.error());
    let (allow, dir) = globals::with(|g| (g.allow_local_infile, g.local_infile_directory.clone()));
    conn.set_option(MYSQL_OPT_LOCAL_INFILE, OptionValue::Int(i64::from(allow)));
    if let Some(d) = dir {
        if check_open_basedir(vm, &d)? {
            conn.set_option(MYSQL_OPT_LOAD_DATA_LOCAL_DIR, OptionValue::Str(&d));
        }
    }
    connect_end(vm, object, my, persistent, new_connection, is_real_connect)
}

/// `php_check_open_basedir()`: whether `path` is within `open_basedir`
/// (warning when not).
fn check_open_basedir(vm: &mut Vm, path: &[u8]) -> VmResult<bool> {
    let Some(basedir) = vm.ini_string("open_basedir").filter(|b| !b.is_empty()) else {
        return Ok(true);
    };
    if basedir
        .split(|&c| c == b':')
        .filter(|d| !d.is_empty())
        .any(|d| php_streams::paths::in_specific_basedir(d, path))
    {
        return Ok(true);
    }
    let mut m = b"open_basedir restriction in effect. File(".to_vec();
    m.extend_from_slice(path);
    m.extend_from_slice(b") is not within the allowed path(s): (");
    m.extend_from_slice(&basedir);
    m.push(b')');
    vm.php_warning_bytes(&m)?;
    Ok(false)
}

fn connect_end(
    vm: &mut Vm,
    object: Option<php_runtime::Obj>,
    my: Rc<RefCell<MyMysql>>,
    persistent: bool,
    new_connection: bool,
    is_real_connect: bool,
) -> VmResult<Value> {
    if persistent && (new_connection || is_real_connect) {
        globals::with(|g| g.num_active_persistent += 1);
    }
    globals::with(|g| g.num_links += 1);
    my.borrow_mut().multi_query = false;
    let res = LinkRes {
        status: Status::Valid,
        my,
    };
    if let Some(o) = object {
        let had = internal::<LinkObj, _>(&o, |l| {
            if let Some(r) = &mut l.0 {
                r.status = Status::Valid;
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
        if !had {
            set_internal(&o, LinkObj(Some(res)));
        }
        Ok(Value::Bool(true))
    } else {
        let o = new_object(vm, "mysqli")?;
        set_internal(&o, LinkObj(Some(res)));
        Ok(Value::Object(o))
    }
}

fn connect_err(my: &Rc<RefCell<MyMysql>>, self_alloced: bool) -> VmResult<Value> {
    let mut m = my.borrow_mut();
    if m.hash_key.is_some() {
        m.hash_key = None;
        m.persistent = false;
    }
    let _ = self_alloced;
    Ok(Value::Bool(false))
}

/// `mysqli_connect_errno()`
pub fn connect_errno(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(globals::with(|g| g.error_no)))
}

/// `mysqli_connect_error()`
pub fn connect_error(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(globals::with(|g| g.error_msg.clone()).map_or(Value::Null, |m| bstr(&m)))
}

/// `mysqli_close()`
pub fn close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = recv(vm, a)?;
    fetch_link(vm, &o, Status::Initialized)?;
    let res = internal::<LinkObj, _>(&o, |l| l.0.take()).flatten();
    if let Some(res) = res {
        close_link(&mut res.my.borrow_mut(), CloseType::Explicit, res.status);
    }
    flush_diag(vm)?;
    Ok(Value::Bool(true))
}

/// Shorthand: the connection of a valid link.
fn link(vm: &mut Vm, a: &Args) -> VmResult<(Rc<RefCell<MyMysql>>, Connection, usize)> {
    let (o, base) = recv(vm, a)?;
    let (my, c) = fetch_link(vm, &o, Status::Valid)?;
    Ok((my, c, base))
}

/// Runs a fallible mysqlnd command and reports its error.
fn bool_cmd(vm: &mut Vm, conn: &Connection, r: &php_mysqlnd::Result<()>) -> VmResult<Value> {
    if r.is_err() {
        report_conn_error(vm, conn)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    Ok(Value::Bool(true))
}

/// `mysqli_affected_rows()`
pub fn affected_rows(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(affected(c.affected_rows()))
}

/// `mysqli_autocommit()`
pub fn autocommit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let on = a.bool(b);
    let r = c.autocommit(on);
    bool_cmd(vm, &c, &r)
}

fn tx_flags(a: &Args, n: usize) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let f = a.int(n) as u32;
    f
}

/// `mysqli_begin_transaction()`
pub fn begin_transaction(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    if a.int(b) < 0 {
        return Err(a.value_error(vm, pos(a, 2), "must be one of the MYSQLI_TRANS_* constants"));
    }
    let name = opt_str(a, b + 1);
    if name.as_ref().is_some_and(ZStr::is_empty) {
        return Err(a.value_error(vm, pos(a, 3), "must not be empty"));
    }
    let r = c.begin_transaction(tx_flags(a, b), name.as_ref().map(ZStr::as_bytes));
    bool_cmd(vm, &c, &r)
}

/// `mysqli_commit()`
pub fn commit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let name = opt_str(a, b + 1);
    let r = c.commit(tx_flags(a, b), name.as_ref().map(ZStr::as_bytes));
    bool_cmd(vm, &c, &r)
}

/// `mysqli_rollback()`
pub fn rollback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let name = opt_str(a, b + 1);
    let r = c.rollback(tx_flags(a, b), name.as_ref().map(ZStr::as_bytes));
    bool_cmd(vm, &c, &r)
}

/// `mysqli_savepoint()`
pub fn savepoint(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let name = a.str(b);
    if name.is_empty() {
        return Err(a.value_error(vm, pos(a, 2), "must not be empty"));
    }
    let r = c.savepoint(Some(name.as_bytes()));
    bool_cmd(vm, &c, &r)
}

/// `mysqli_release_savepoint()`
pub fn release_savepoint(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let name = a.str(b);
    if name.is_empty() {
        return Err(a.value_error(vm, pos(a, 2), "must not be empty"));
    }
    let r = c.release_savepoint(Some(name.as_bytes()));
    bool_cmd(vm, &c, &r)
}

/// `mysqli_change_user()`
pub fn change_user(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let user = a.str(b);
    let pw = a.str(b + 1);
    let db = opt_str(a, b + 2);
    let r = c.change_user(
        user.as_bytes(),
        pw.as_bytes(),
        db.as_ref().map(ZStr::as_bytes),
        false,
    );
    report_conn_error(vm, &c)?;
    Ok(Value::Bool(r.is_ok()))
}

/// `mysqli_character_set_name()`
pub fn character_set_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Str(ZStr::from(c.character_set_name().unwrap_or(""))))
}

/// `mysqli_debug()`
pub fn debug(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(true))
}

/// `mysqli_dump_debug_info()`
pub fn dump_debug_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    let r = c.dump_debug_info();
    flush_diag(vm)?;
    Ok(Value::Bool(r.is_ok()))
}

/// `mysqli_errno()`
pub fn errno(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Int(i64::from(c.errno())))
}

/// `mysqli_error()`
pub fn error(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(bstr(&c.error()))
}

/// `mysqli_error_list()`
pub fn mysqli_error_list(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(error_list(&c.error_list()))
}

/// `mysqli_sqlstate()`
pub fn sqlstate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Str(ZStr::from(c.sqlstate())))
}

/// `mysqli_field_count()`
pub fn field_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Int(i64::from(c.field_count())))
}

fn stats_array(pairs: Vec<(&'static str, String)>) -> Value {
    Value::Array(ZArray::from_pairs(
        pairs
            .into_iter()
            .map(|(k, v)| (ArrayKey::from(k), Value::Str(ZStr::from(v)))),
    ))
}

/// `mysqli_get_connection_stats()`
pub fn get_connection_stats(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(stats_array(c.stats().to_pairs()))
}

/// `mysqli_get_client_stats()`
pub fn get_client_stats(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    sync_mysqlnd_ini(vm);
    Ok(stats_array(php_mysqlnd::stats::global().to_pairs()))
}

/// `mysqli_get_links_stats()`
pub fn get_links_stats(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let (t, ap, cp) = globals::with(|g| {
        (
            g.num_links,
            g.num_active_persistent,
            g.num_inactive_persistent,
        )
    });
    Ok(Value::Array(ZArray::from_pairs([
        (ArrayKey::from("total"), Value::Int(t)),
        (ArrayKey::from("active_plinks"), Value::Int(ap)),
        (ArrayKey::from("cached_plinks"), Value::Int(cp)),
    ])))
}

/// `mysqli_get_charset()`
pub fn get_charset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    let Some(cs) = c.charset() else {
        vm.php_warning("The connection has no charset associated")?;
        return Ok(Value::Null);
    };
    crate::util::std_object(
        vm,
        vec![
            ("charset", Value::Str(ZStr::from(cs.name))),
            ("collation", Value::Str(ZStr::from(cs.collation))),
            ("dir", Value::Str(ZStr::from(""))),
            ("min_length", Value::Int(i64::from(cs.char_minlen))),
            ("max_length", Value::Int(i64::from(cs.char_maxlen))),
            ("number", Value::Int(0)),
            ("state", Value::Int(1)),
        ],
    )
}

/// `mysqli_get_client_info()`
pub fn get_client_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if !is_method(a) && a.has(0) {
        vm.php_error_bytes(
            E_DEPRECATED,
            b"Passing connection object as an argument is deprecated",
        )?;
    }
    Ok(Value::Str(ZStr::from(php_mysqlnd::CLIENT_INFO)))
}

/// `mysqli_get_client_version()`
pub fn get_client_version(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    #[allow(clippy::cast_possible_wrap)]
    Ok(Value::Int(php_mysqlnd::CLIENT_VERSION as i64))
}

/// `mysqli_get_host_info()`
pub fn get_host_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Str(ZStr::from(c.host_info().unwrap_or_default())))
}

/// `mysqli_get_proto_info()`
pub fn get_proto_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Int(i64::from(c.proto_info())))
}

/// `mysqli_get_server_info()`
pub fn get_server_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Str(ZStr::from(c.server_info().unwrap_or_default())))
}

/// `mysqli_get_server_version()`
pub fn get_server_version(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(long_or_string(c.server_version()))
}

/// `mysqli_info()`
pub fn info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(c.info().map_or(Value::Null, |i| bstr(&i)))
}

/// `mysqli_insert_id()`
pub fn insert_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(long_or_string(c.insert_id()))
}

/// `mysqli_kill()`
pub fn kill(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let pid = a.int(b);
    if pid <= 0 {
        return Err(a.value_error(vm, pos(a, 2), "must be greater than 0"));
    }
    let (_, c) = fetch_link(vm, &o, Status::Valid)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let r = c.kill(pid as u32);
    bool_cmd(vm, &c, &r)
}

/// `mysqli_more_results()`
pub fn more_results(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Bool(c.more_results()))
}

/// `mysqli_next_result()`
pub fn next_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    if let Ok(true) = c.next_result() {
        flush_diag(vm)?;
        Ok(Value::Bool(true))
    } else {
        report_conn_error(vm, &c)?;
        Ok(Value::Bool(false))
    }
}

/// `mysqli_multi_query()`
pub fn multi_query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (my, c, b) = link(vm, a)?;
    let q = a.str(b);
    crate::stmt::flush_buried(&c);
    if !my.borrow().multi_query {
        let _ = c.set_server_option(OPTION_MULTI_STATEMENTS_ON);
        my.borrow_mut().multi_query = true;
    }
    if c.query(q.as_bytes()).is_err() {
        // MYSQLI_DISABLE_MQ would clear the error (mysqlnd offers no way
        // to restore it), so multi statements are switched off lazily by
        // the next mysqli_query()/real_query().
        report_conn_error(vm, &c)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    Ok(Value::Bool(true))
}

/// `mysqli_options()` / `mysqli_set_opt()`
pub fn options(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    use php_mysqlnd::consts::opt::{
        MYSQLND_OPT_INT_AND_FLOAT_NATIVE, MYSQLND_OPT_NET_CMD_BUFFER_SIZE,
        MYSQLND_OPT_NET_READ_BUFFER_SIZE, MYSQL_INIT_COMMAND,
        MYSQL_OPT_CAN_HANDLE_EXPIRED_PASSWORDS, MYSQL_OPT_COMPRESS, MYSQL_OPT_CONNECT_TIMEOUT,
        MYSQL_OPT_NAMED_PIPE, MYSQL_OPT_PROTOCOL, MYSQL_OPT_READ_TIMEOUT,
        MYSQL_OPT_SSL_VERIFY_SERVER_CERT, MYSQL_OPT_WRITE_TIMEOUT, MYSQL_READ_DEFAULT_FILE,
        MYSQL_READ_DEFAULT_GROUP, MYSQL_SERVER_PUBLIC_KEY, MYSQL_SET_CHARSET_NAME,
    };
    let (o, b) = recv(vm, a)?;
    let (_, c) = fetch_link(vm, &o, Status::Initialized)?;
    let opt = a.int(b);
    let Ok(opt32) = u32::try_from(opt) else {
        return Err(a.value_error(vm, pos(a, 2), "must be MYSQLI_INIT_COMMAND, MYSQLI_SET_CHARSET_NAME, MYSQLI_SERVER_PUBLIC_KEY, or one of the MYSQLI_OPT_* constants"));
    };
    let is_long = matches!(
        opt32,
        MYSQLND_OPT_NET_CMD_BUFFER_SIZE
            | MYSQLND_OPT_NET_READ_BUFFER_SIZE
            | MYSQLND_OPT_INT_AND_FLOAT_NATIVE
            | MYSQL_OPT_CONNECT_TIMEOUT
            | MYSQL_OPT_LOCAL_INFILE
            | MYSQL_OPT_NAMED_PIPE
            | MYSQL_OPT_PROTOCOL
            | MYSQL_OPT_READ_TIMEOUT
            | MYSQL_OPT_WRITE_TIMEOUT
            | MYSQL_OPT_SSL_VERIFY_SERVER_CERT
            | MYSQL_OPT_COMPRESS
            | MYSQL_OPT_CAN_HANDLE_EXPIRED_PASSWORDS
    );
    let is_str = matches!(
        opt32,
        MYSQL_READ_DEFAULT_FILE
            | MYSQL_READ_DEFAULT_GROUP
            | MYSQL_INIT_COMMAND
            | MYSQL_SET_CHARSET_NAME
            | MYSQL_SERVER_PUBLIC_KEY
            | MYSQL_OPT_LOAD_DATA_LOCAL_DIR
    );
    if !is_long && !is_str {
        return Err(a.value_error(vm, pos(a, 2), "must be MYSQLI_INIT_COMMAND, MYSQLI_SET_CHARSET_NAME, MYSQLI_SERVER_PUBLIC_KEY, or one of the MYSQLI_OPT_* constants"));
    }
    let v = a.at(b + 1);
    let ok = if is_str {
        let s = vm.value_to_string(&v)?;
        c.set_option(opt32, OptionValue::Str(s.as_bytes()))
    } else {
        let i = match &v {
            Value::Int(i) => *i,
            other => vm.value_to_int(other)?,
        };
        // `unsigned int l_value = Z_LVAL_P(...)`
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let l = i64::from(i as u32);
        c.set_option(opt32, OptionValue::Int(l))
    };
    if !ok {
        report_conn_error(vm, &c)?;
    }
    flush_diag(vm)?;
    Ok(Value::Bool(ok))
}

/// `mysqli_ping()`
pub fn ping(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    let r = c.ping();
    report_conn_error(vm, &c)?;
    Ok(Value::Bool(r.is_ok()))
}

/// `mysqli_query()`
pub fn query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let q = a.str(b);
    let mode = a.int(b + 1);
    if q.is_empty() {
        return Err(a.value_error(vm, pos(a, 2), "must not be empty"));
    }
    if (mode & !ASYNC) != USE_RESULT && STORE_RESULT != (mode & !(ASYNC | STORE_RESULT_COPY_DATA)) {
        return Err(a.value_error(
            vm,
            pos(a, 3),
            "must be either MYSQLI_USE_RESULT or MYSQLI_STORE_RESULT with MYSQLI_ASYNC as an optional bitmask flag",
        ));
    }
    let (my, c) = fetch_link(vm, &o, Status::Valid)?;
    crate::stmt::flush_buried(&c);
    disable_mq(&my, &c);
    if mode & ASYNC != 0 {
        if c.send_query(q.as_bytes()).is_err() {
            report_conn_error(vm, &c)?;
            return Ok(Value::Bool(false));
        }
        flush_diag(vm)?;
        my.borrow_mut().async_result_fetch_type = mode & !ASYNC;
        return Ok(Value::Bool(true));
    }
    if c.query(q.as_bytes()).is_err() {
        report_conn_error(vm, &c)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    if c.field_count() == 0 {
        if globals::report_mode() & REPORT_INDEX != 0 {
            report_index(vm, q.as_bytes(), c.server_status())?;
        }
        return Ok(Value::Bool(true));
    }
    let r = if mode & !(ASYNC | STORE_RESULT_COPY_DATA) == USE_RESULT {
        c.use_result()
    } else {
        c.store_result()
    };
    finish_result(vm, &c, r, Some(q.as_bytes()))
}

/// Wraps a fetched result (or reports the error).
fn finish_result(
    vm: &mut Vm,
    c: &Connection,
    r: php_mysqlnd::Result<Option<php_mysqlnd::ResultSet>>,
    index_query: Option<&[u8]>,
) -> VmResult<Value> {
    let Ok(Some(rs)) = r else {
        report_conn_error(vm, c)?;
        return Ok(Value::Bool(false));
    };
    flush_diag(vm)?;
    if let Some(q) = index_query {
        if globals::report_mode() & REPORT_INDEX != 0 {
            report_index(vm, q, c.server_status())?;
        }
    }
    new_result_object(vm, rs)
}

/// `mysqli_real_query()`
pub fn real_query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (my, c, b) = link(vm, a)?;
    let q = a.str(b);
    crate::stmt::flush_buried(&c);
    disable_mq(&my, &c);
    if c.query(q.as_bytes()).is_err() {
        report_conn_error(vm, &c)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    if c.field_count() == 0 && globals::report_mode() & REPORT_INDEX != 0 {
        report_index(vm, q.as_bytes(), c.server_status())?;
    }
    Ok(Value::Bool(true))
}

/// `mysqli_reap_async_query()`
pub fn reap_async_query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (my, c, _) = link(vm, a)?;
    if c.reap_query().is_err() {
        report_conn_error(vm, &c)?;
        return Ok(Value::Bool(false));
    }
    flush_diag(vm)?;
    if c.field_count() == 0 {
        return Ok(Value::Bool(true));
    }
    let mode = my.borrow().async_result_fetch_type;
    let r = match mode {
        STORE_RESULT => c.store_result(),
        USE_RESULT => c.use_result(),
        _ => Ok(None),
    };
    finish_result(vm, &c, r, None)
}

/// `mysqli_store_result()`
pub fn store_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = recv(vm, a)?;
    let n = a.passed.iter().filter(|p| **p).count();
    if (is_method(a) && n == 1) || n == 2 {
        vm.raise(
            E_DEPRECATED,
            "Passing the $mode parameter is deprecated since 8.4, as it has been ignored since 8.1",
        )?;
    }
    let (_, c) = fetch_link(vm, &o, Status::Valid)?;
    let r = c.store_result();
    finish_result(vm, &c, r, Some(b"from previous query"))
}

/// `mysqli_use_result()`
pub fn use_result(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    let r = c.use_result();
    finish_result(vm, &c, r, Some(b"from previous query"))
}

/// `mysqli_real_escape_string()` / `mysqli_escape_string()`
pub fn real_escape_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let s = a.str(b);
    Ok(bstr(&c.escape_string(s.as_bytes())))
}

/// `mysqli_quote_string()`
pub fn quote_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let s = a.str(b);
    let mut out = vec![b'\''];
    out.extend_from_slice(&c.escape_string(s.as_bytes()));
    out.push(b'\'');
    Ok(bstr(&out))
}

/// `mysqli_select_db()`
pub fn select_db(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let db = a.str(b);
    let r = c.select_db(db.as_bytes());
    bool_cmd(vm, &c, &r)
}

/// `mysqli_set_charset()`
pub fn set_charset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, b) = link(vm, a)?;
    let cs = a.str(b);
    let r = c.set_charset(&cs.to_string_lossy());
    bool_cmd(vm, &c, &r)
}

/// `mysqli_ssl_set()`
pub fn ssl_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let (_, c) = fetch_link(vm, &o, Status::Initialized)?;
    let p: Vec<Option<String>> = (0..5)
        .map(|i| {
            opt_str(a, b + i)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string_lossy())
        })
        .collect();
    c.ssl_set(
        p[0].as_deref(),
        p[1].as_deref(),
        p[2].as_deref(),
        p[3].as_deref(),
        p[4].as_deref(),
    );
    Ok(Value::Bool(true))
}

/// `mysqli_stat()`
pub fn stat(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    let r = c.statistics();
    flush_diag(vm)?;
    Ok(r.map_or(Value::Bool(false), |s| bstr(&s)))
}

/// `mysqli_refresh()`
pub fn refresh(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, b) = recv(vm, a)?;
    let (_, c) = fetch_link(vm, &o, Status::Initialized)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let opts = a.int(b) as u8;
    let r = c.refresh(opts);
    flush_diag(vm)?;
    Ok(Value::Bool(r.is_ok()))
}

/// `mysqli_thread_id()`
pub fn thread_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(long_or_string(c.thread_id()))
}

/// `mysqli_thread_safe()`
pub fn thread_safe(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(true))
}

/// `mysqli_warning_count()`
pub fn warning_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    Ok(Value::Int(i64::from(c.warning_count())))
}

/// `mysqli_report()`
pub fn report(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let f = a.int(0);
    globals::with(|g| g.report_mode = f);
    Ok(Value::Bool(true))
}

/// Collects the connections of a `mysqli_poll()` array (`mysqlnd_zval_array_to_mysqlnd_array`).
fn poll_array(vm: &mut Vm, arr: &ZArray) -> VmResult<Vec<(Value, Connection)>> {
    let mut out = Vec::new();
    for (i, v) in arr.values().enumerate() {
        let v = v.deref();
        let Value::Object(o) = &v else {
            let msg = format!(
                "mysqli_poll(): Argument #{} must be an instance of mysqli, {} given",
                i + 1,
                crate::util::type_name(&v)
            );
            return Err(vm.throw("TypeError", &msg));
        };
        if !php_vm::object::class_of(o).instance_of_name(b"mysqli") {
            let msg = format!(
                "mysqli_poll(): Argument #{} must be an instance of mysqli, {} given",
                i + 1,
                crate::objects::class_name(o)
            );
            return Err(vm.throw("TypeError", &msg));
        }
        let (_, c) = fetch_link(vm, o, Status::Valid)?;
        out.push((v.clone(), c));
    }
    Ok(out)
}

/// `mysqli_poll()`
pub fn poll(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let sec = a.int(3);
    let usec = a.int(4);
    if sec < 0 {
        return Err(a.value_error(vm, 4, "must be greater than or equal to 0"));
    }
    if usec < 0 {
        return Err(a.value_error(vm, 5, "must be greater than or equal to 0"));
    }
    let r_arr = match a.at(0) {
        Value::Array(x) => Some(x),
        _ => None,
    };
    let e_arr = match a.at(1) {
        Value::Array(x) => Some(x),
        _ => None,
    };
    if r_arr.is_none() && e_arr.is_none() {
        return Err(vm.throw("ValueError", "No stream arrays were passed"));
    }
    let r = match &r_arr {
        Some(x) => poll_array(vm, x)?,
        None => Vec::new(),
    };
    let e = match &e_arr {
        Some(x) => poll_array(vm, x)?,
        None => Vec::new(),
    };
    let rc: Vec<Connection> = r.iter().map(|(_, c)| c.clone()).collect();
    let ec: Vec<Connection> = e.iter().map(|(_, c)| c.clone()).collect();
    let res = php_mysqlnd::poll(&rc, &ec, sec, usec);
    flush_diag(vm)?;
    let pick = |list: &[(Value, Connection)], idx: &[usize]| {
        Value::Array(ZArray::from_values(idx.iter().map(|&i| list[i].0.clone())))
    };
    let dont = res
        .as_ref()
        .map(|p| p.dont_poll.clone())
        .unwrap_or_default();
    a.set_ref(
        2,
        if r_arr.is_some() {
            pick(&r, &dont)
        } else {
            Value::Array(ZArray::new())
        },
    );
    if let Some(p) = res {
        if r_arr.is_some() {
            a.set_ref(0, pick(&r, &p.read));
        }
        if e_arr.is_some() {
            a.set_ref(1, pick(&e, &p.error));
        }
        Ok(Value::Int(i64::try_from(p.ready).unwrap_or(0)))
    } else {
        if r_arr.is_some() {
            a.set_ref(0, Value::Array(ZArray::new()));
        }
        if e_arr.is_some() {
            a.set_ref(1, Value::Array(ZArray::new()));
        }
        Ok(Value::Bool(false))
    }
}

/// `php_get_warnings`: runs `SHOW WARNINGS`.
fn fetch_warnings(conn: &Connection) -> Vec<crate::objects::Warning> {
    let mut out = Vec::new();
    if conn.query(b"SHOW WARNINGS").is_err() {
        return out;
    }
    let Ok(Some(mut rs)) = conn.use_result() else {
        return out;
    };
    while let Ok(Some(row)) = rs.fetch_row_values() {
        let errno = row.get(1).map_or(0, |v| match v {
            Value::Int(i) => *i,
            Value::Str(s) => s.to_string_lossy().trim().parse().unwrap_or(0),
            _ => 0,
        });
        let msg = match row.get(2) {
            Some(Value::Str(s)) => Value::Str(s.clone()),
            Some(Value::Int(i)) => Value::Str(ZStr::from(i.to_string())),
            _ => Value::Str(ZStr::from("")),
        };
        out.push(crate::objects::Warning {
            message: msg,
            sqlstate: "HY000",
            errno,
        });
    }
    out
}

/// Builds a `mysqli_warning` object (or `false` without warnings).
pub fn warnings_object(vm: &mut Vm, conn: &Connection) -> VmResult<Value> {
    let w = fetch_warnings(conn);
    flush_diag(vm)?;
    if w.is_empty() {
        return Ok(Value::Bool(false));
    }
    let o = new_object(vm, "mysqli_warning")?;
    set_internal(&o, crate::objects::WarningObj(Some((w, 0))));
    Ok(Value::Object(o))
}

/// `mysqli_get_warnings()`
pub fn get_warnings(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, c, _) = link(vm, a)?;
    if c.warning_count() == 0 {
        return Ok(Value::Bool(false));
    }
    warnings_object(vm, &c)
}

/// `mysqli_warning::next()`
pub fn warning_next(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let moved = internal::<crate::objects::WarningObj, _>(&o, |w| match &mut w.0 {
        Some((list, i)) if *i + 1 < list.len() => {
            *i += 1;
            true
        }
        _ => false,
    })
    .unwrap_or(false);
    Ok(Value::Bool(moved))
}

/// `mysqli_warning::__construct()`
pub fn warning_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", "Cannot directly construct mysqli_warning"))
}

//! The virtual properties of the mysqli classes (ref: `mysqli_prop.c`,
//! `mysqli_driver.c`, `mysqli_warning.c`, `mysqli.c` property handlers):
//! reads, `isset()`/`empty()`, read-only writes and the `var_dump()`
//! table (`mysqli_object_get_debug_info`).

use php_runtime::{ArrayKey, HashTable, Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::object::{class_of, internal};
use php_vm::{Vm, VmResult};

use crate::globals;
use crate::objects::{class_name, LinkObj, ResultObj, Status, StmtObj, WarningObj};
use crate::util::{affected, bstr, error_list, long_or_string};

/// `MYSQLI_VERSION_ID`
const MYSQLI_VERSION_ID: i64 = 101_009;

/// Which property table an object uses (its internal base class).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `mysqli_driver`
    Driver,
    /// `mysqli`
    Link,
    /// `mysqli_result`
    Result,
    /// `mysqli_stmt`
    Stmt,
    /// `mysqli_warning`
    Warning,
}

const DRIVER_PROPS: &[&str] = &["client_info", "client_version", "driver_version", "report_mode"];
const LINK_PROPS: &[&str] = &[
    "affected_rows",
    "client_info",
    "client_version",
    "connect_errno",
    "connect_error",
    "errno",
    "error",
    "error_list",
    "field_count",
    "host_info",
    "info",
    "insert_id",
    "server_info",
    "server_version",
    "sqlstate",
    "protocol_version",
    "thread_id",
    "warning_count",
];
const RESULT_PROPS: &[&str] = &["current_field", "field_count", "lengths", "num_rows", "type"];
const STMT_PROPS: &[&str] = &[
    "affected_rows",
    "insert_id",
    "num_rows",
    "param_count",
    "field_count",
    "errno",
    "error",
    "error_list",
    "sqlstate",
    "id",
];
const WARNING_PROPS: &[&str] = &["message", "sqlstate", "errno"];

fn table(k: Kind) -> &'static [&'static str] {
    match k {
        Kind::Driver => DRIVER_PROPS,
        Kind::Link => LINK_PROPS,
        Kind::Result => RESULT_PROPS,
        Kind::Stmt => STMT_PROPS,
        Kind::Warning => WARNING_PROPS,
    }
}

fn kind_of(o: &Obj) -> Option<Kind> {
    let c = class_of(o);
    if c.instance_of_name(b"mysqli") {
        Some(Kind::Link)
    } else if c.instance_of_name(b"mysqli_result") {
        Some(Kind::Result)
    } else if c.instance_of_name(b"mysqli_stmt") {
        Some(Kind::Stmt)
    } else if c.instance_of_name(b"mysqli_driver") {
        Some(Kind::Driver)
    } else if c.instance_of_name(b"mysqli_warning") {
        Some(Kind::Warning)
    } else {
        None
    }
}

/// Why a read failed (the error is thrown unless the read is quiet).
enum Fail {
    NotAllowed,
    Closed,
    NotInit,
    Quiet,
}

type Read = Result<Value, Fail>;

fn link_read(vm: &mut Vm, o: &Obj, name: &str, quiet: bool) -> VmResult<Read> {
    let state = internal::<LinkObj, _>(o, |l| l.0.as_ref().map(|r| (r.status, r.my.borrow().mysql.clone()))).flatten();
    let need_valid = |st: Option<Status>, need: Status| st.is_some_and(|s| s >= need);
    match name {
        "client_info" => return Ok(Ok(Value::Str(ZStr::from(php_mysqlnd::CLIENT_INFO)))),
        "client_version" => {
            #[allow(clippy::cast_possible_wrap)]
            return Ok(Ok(Value::Int(php_mysqlnd::CLIENT_VERSION as i64)));
        }
        "connect_errno" => return Ok(Ok(Value::Int(globals::with(|g| g.error_no)))),
        "connect_error" => {
            return Ok(Ok(globals::with(|g| g.error_msg.clone()).map_or(Value::Null, |m| bstr(&m))));
        }
        "affected_rows" | "error_list" => {
            let st = state.as_ref().map(|s| s.0);
            if !need_valid(st, Status::Valid) {
                return Ok(Err(Fail::NotAllowed));
            }
            let conn = state.and_then(|s| s.1);
            return Ok(Ok(match (name, conn) {
                ("affected_rows", Some(c)) => affected(c.affected_rows()),
                ("affected_rows", None) => Value::Int(-1),
                (_, Some(c)) => error_list(&c.error_list()),
                (_, None) => Value::Array(ZArray::new()),
            }));
        }
        _ => {}
    }
    let need = if matches!(name, "errno" | "error") {
        Status::Initialized
    } else {
        Status::Valid
    };
    let Some((status, conn)) = state else {
        return Ok(Err(Fail::Closed));
    };
    if status < need {
        return Ok(Err(Fail::NotAllowed));
    }
    let Some(c) = conn else {
        return Ok(Err(Fail::NotInit));
    };
    let _ = (vm, quiet);
    Ok(Ok(match name {
        "errno" => Value::Int(i64::from(c.errno())),
        "error" => bstr(&c.error()),
        "field_count" => Value::Int(i64::from(c.field_count())),
        "host_info" => Value::Str(ZStr::from(c.host_info().unwrap_or_default())),
        "info" => c.info().map_or(Value::Null, |i| bstr(&i)),
        "insert_id" => long_or_string(c.insert_id()),
        "server_info" => Value::Str(ZStr::from(c.server_info().unwrap_or_default())),
        "server_version" => long_or_string(c.server_version()),
        "sqlstate" => Value::Str(ZStr::from(c.sqlstate())),
        "protocol_version" => Value::Int(i64::from(c.proto_info())),
        "thread_id" => long_or_string(c.thread_id()),
        "warning_count" => Value::Int(i64::from(c.warning_count())),
        _ => Value::Null,
    }))
}

fn result_read(o: &Obj, name: &str) -> Read {
    let state = internal::<ResultObj, _>(o, |r| r.0.as_ref().map(|r| (r.status, r.rs.clone()))).flatten();
    let Some((status, rs)) = state else {
        return Err(if matches!(name, "lengths" | "type") {
            Fail::NotAllowed
        } else {
            Fail::Closed
        });
    };
    if status < Status::Valid {
        return Err(Fail::NotAllowed);
    }
    let r = rs.borrow();
    Ok(match name {
        "current_field" => Value::Int(i64::from(r.field_tell())),
        "field_count" => Value::Int(i64::from(r.num_fields())),
        "lengths" => {
            if r.num_fields() == 0 {
                Value::Null
            } else {
                r.fetch_lengths()
                    .map_or(Value::Null, |l| crate::result::lengths_array(&l))
            }
        }
        "num_rows" => long_or_string(r.num_rows()),
        "type" => Value::Int(i64::from(!r.is_buffered())),
        _ => Value::Null,
    })
}

fn stmt_read(o: &Obj, name: &str) -> Read {
    let state = internal::<StmtObj, _>(o, |r| r.0.as_ref().map(|r| (r.status, r.st.borrow().stmt.clone()))).flatten();
    let check_only = matches!(name, "affected_rows" | "error_list" | "id");
    let Some((status, st)) = state else {
        return Err(if check_only { Fail::NotAllowed } else { Fail::Closed });
    };
    let need = if matches!(name, "errno" | "error" | "error_list" | "sqlstate") {
        Status::Initialized
    } else {
        Status::Valid
    };
    if status < need {
        return Err(Fail::NotAllowed);
    }
    Ok(match name {
        "affected_rows" => affected(st.affected_rows()),
        "insert_id" => long_or_string(st.insert_id()),
        "num_rows" => long_or_string(st.num_rows()),
        "param_count" => Value::Int(i64::from(st.param_count())),
        "field_count" => Value::Int(i64::from(st.field_count())),
        "errno" => Value::Int(i64::from(st.errno())),
        "error" => bstr(&st.error()),
        "error_list" => error_list(&st.error_list()),
        "sqlstate" => Value::Str(ZStr::from(st.sqlstate())),
        "id" => Value::Int(i64::from(st.id())),
        _ => Value::Null,
    })
}

fn warning_read(o: &Obj, name: &str) -> Read {
    let w = internal::<WarningObj, _>(o, |w| w.0.as_ref().and_then(|(l, i)| l.get(*i).cloned())).flatten();
    let Some(w) = w else {
        return Err(Fail::Closed);
    };
    Ok(match name {
        "message" => w.message,
        "sqlstate" => Value::Str(ZStr::from(w.sqlstate)),
        _ => Value::Int(w.errno),
    })
}

fn driver_read(vm: &mut Vm, name: &str, quiet: bool) -> VmResult<Read> {
    Ok(match name {
        "client_info" => Ok(Value::Str(ZStr::from(php_mysqlnd::CLIENT_INFO))),
        "client_version" => {
            #[allow(clippy::cast_possible_wrap)]
            Ok(Value::Int(php_mysqlnd::CLIENT_VERSION as i64))
        }
        "driver_version" => {
            if quiet {
                Err(Fail::Quiet)
            } else {
                vm.raise(crate::report::E_DEPRECATED, "The driver_version property is deprecated")?;
                Ok(Value::Int(MYSQLI_VERSION_ID))
            }
        }
        _ => Ok(Value::Int(globals::report_mode())),
    })
}

/// Reads a virtual property: `Ok(None)` when `name` is not one.
fn read(vm: &mut Vm, o: &Obj, name: &ZStr, quiet: bool) -> VmResult<Option<Read>> {
    let Some(k) = kind_of(o) else { return Ok(None) };
    let n = name.to_string_lossy();
    if !table(k).contains(&n.as_str()) {
        return Ok(None);
    }
    let r = match k {
        Kind::Driver => driver_read(vm, &n, quiet)?,
        Kind::Link => link_read(vm, o, &n, quiet)?,
        Kind::Result => result_read(o, &n),
        Kind::Stmt => stmt_read(o, &n),
        Kind::Warning => warning_read(o, &n),
    };
    Ok(Some(r))
}

fn throw_fail(vm: &mut Vm, o: &Obj, f: &Fail) -> php_vm::Throw {
    let msg = match f {
        Fail::NotAllowed => "Property access is not allowed yet".to_string(),
        Fail::Closed => format!("{} object is already closed", class_name(o)),
        Fail::NotInit => format!("{} object is not fully initialized", class_name(o)),
        Fail::Quiet => "Cannot read property".to_string(),
    };
    vm.throw("Error", &msg)
}

/// `read_property` handler.
pub fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    crate::report::flush_diag_main(vm)?;
    match read(vm, o, name, false)? {
        None => Ok(None),
        Some(Ok(v)) => Ok(Some(v)),
        Some(Err(f)) => Err(throw_fail(vm, o, &f)),
    }
}

/// `has_property` handler: the result of `isset()`, or of `empty()` when
/// `check_empty` is set (the VM's convention).
pub fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    match read(vm, o, name, true)? {
        None => Ok(None),
        Some(Err(_)) => Ok(Some(check_empty)),
        Some(Ok(v)) => {
            if check_empty {
                Ok(Some(!vm.value_to_bool(&v)?))
            } else {
                Ok(Some(!matches!(v, Value::Null)))
            }
        }
    }
}

/// `write_property` handler: the virtual properties are read-only except
/// `mysqli_driver::$report_mode`.
pub fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, v: &Value) -> VmResult<bool> {
    let Some(k) = kind_of(o) else { return Ok(false) };
    let n = name.to_string_lossy();
    if !table(k).contains(&n.as_str()) {
        return Ok(false);
    }
    if k == Kind::Driver && n == "report_mode" {
        let strict = vm
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != php_vm::vm::FrameKind::Builtin)
            .is_some_and(|f| f.code.strict_types);
        let mode = match v.deref() {
            Value::Int(i) => i,
            other if strict => {
                let msg = format!(
                    "Cannot assign {} to property mysqli_driver::$report_mode of type int",
                    crate::util::type_name(&other)
                );
                return Err(vm.throw("TypeError", &msg));
            }
            Value::Bool(b) => i64::from(b),
            Value::Float(f) if f.fract() == 0.0 && f.is_finite() => {
                #[allow(clippy::cast_possible_truncation)]
                let i = f as i64;
                i
            }
            Value::Str(s) if s.to_string_lossy().trim().parse::<i64>().is_ok() => {
                s.to_string_lossy().trim().parse::<i64>().unwrap_or(0)
            }
            other => {
                let msg = format!(
                    "Cannot assign {} to property mysqli_driver::$report_mode of type int",
                    crate::util::type_name(&other)
                );
                return Err(vm.throw("TypeError", &msg));
            }
        };
        globals::with(|g| g.report_mode = mode);
        return Ok(true);
    }
    let msg = format!("Cannot write read-only property {}::${n}", class_name(o));
    Err(vm.throw("Error", &msg))
}

/// `get_debug_info`: the readable virtual properties (`var_dump()`).
pub fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    let Some(k) = kind_of(o) else { return Ok(None) };
    let mut t = HashTable::new();
    for name in table(k) {
        if let Some(Ok(v)) = read(vm, o, &ZStr::from(*name), true)? {
            t.insert(ArrayKey::from(*name), v);
        }
    }
    Ok(Some(ZArray::from_table(t)))
}

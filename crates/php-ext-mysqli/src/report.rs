//! Error reporting (ref: `mysqli_report.c`, `mysqli_exception.c`,
//! `MYSQLI_REPORT_MYSQL_ERROR` / `MYSQLI_REPORT_STMT_ERROR`): turns mysqlnd
//! errors into warnings or `mysqli_sql_exception`s according to the report
//! mode, and forwards the diagnostics queued by mysqlnd.

use php_mysqlnd::consts::{SERVER_QUERY_NO_GOOD_INDEX_USED, SERVER_QUERY_NO_INDEX_USED};
use php_mysqlnd::{diag, Connection, Statement};
use php_runtime::{RuntimeError, Value, ZStr};
use php_vm::{Throw, Vm, VmResult};

use crate::globals::{self, REPORT_ERROR, REPORT_STRICT};

/// `E_ERROR`
pub const E_ERROR: i64 = 1;
/// `E_WARNING`
pub const E_WARNING: i64 = 2;
/// `E_DEPRECATED`
pub const E_DEPRECATED: i64 = 8192;

/// Emits queued diagnostics outside a native function (property
/// handlers): `php_error_docref` then reports itself as `main()`.
pub fn flush_diag_main(vm: &mut Vm) -> VmResult<()> {
    for d in diag::take() {
        let level = if d.fatal { E_ERROR } else { E_WARNING };
        let mut m = if d.docref { b"main(): ".to_vec() } else { Vec::new() };
        m.extend_from_slice(d.message.as_bytes());
        vm.raise_bytes(level, &m)?;
    }
    Ok(())
}

/// Emits the diagnostics mysqlnd queued (`php_error_docref` /
/// `php_error` calls inside the driver).
pub fn flush_diag(vm: &mut Vm) -> VmResult<()> {
    for d in diag::take() {
        let level = if d.fatal { E_ERROR } else { E_WARNING };
        if d.docref {
            vm.php_error_bytes(level, d.message.as_bytes())?;
        } else {
            vm.raise(level, &d.message)?;
        }
    }
    Ok(())
}

/// `php_mysqli_throw_sql_exception`: a warning `(sqlstate/errno): msg`
/// without `MYSQLI_REPORT_STRICT`, else a `mysqli_sql_exception`.
pub fn throw_sql_exception(vm: &mut Vm, sqlstate: &str, errno: i64, msg: &[u8]) -> VmResult<()> {
    if globals::report_mode() & REPORT_STRICT == 0 {
        let mut m = format!("({sqlstate}/{errno}): ").into_bytes();
        m.extend_from_slice(msg);
        return vm.php_warning_bytes(&m);
    }
    let t = vm.throw("mysqli_sql_exception", "");
    if let Throw::Exception(o) = &t {
        vm.set_property_raw(o, "message", Value::Str(ZStr::from(msg)));
        let st = if sqlstate.is_empty() { "00000" } else { sqlstate };
        vm.set_property_raw(o, "sqlstate", Value::Str(ZStr::from(st)));
        vm.set_property_raw(o, "code", Value::Int(errno));
    }
    Err(t)
}

/// `php_mysqli_report_error`
pub fn report_error(vm: &mut Vm, sqlstate: &str, errno: u32, msg: &[u8]) -> VmResult<()> {
    throw_sql_exception(vm, sqlstate, i64::from(errno), msg)
}

/// `MYSQLI_REPORT_MYSQL_ERROR(conn)`
pub fn report_conn_error(vm: &mut Vm, conn: &Connection) -> VmResult<()> {
    flush_diag(vm)?;
    let errno = conn.errno();
    if globals::report_mode() & REPORT_ERROR != 0 && errno != 0 {
        let st = conn.sqlstate();
        let msg = conn.error();
        report_error(vm, &st, errno, &msg)?;
    }
    Ok(())
}

/// `MYSQLI_REPORT_STMT_ERROR(stmt)`
pub fn report_stmt_error(vm: &mut Vm, stmt: &Statement) -> VmResult<()> {
    flush_diag(vm)?;
    let errno = stmt.errno();
    if globals::report_mode() & REPORT_ERROR != 0 && errno != 0 {
        let st = stmt.sqlstate();
        let msg = stmt.error();
        report_error(vm, &st, errno, &msg)?;
    }
    Ok(())
}

/// `php_mysqli_report_index`
pub fn report_index(vm: &mut Vm, query: &[u8], status: u16) -> VmResult<()> {
    let index = if status & SERVER_QUERY_NO_GOOD_INDEX_USED != 0 {
        "Bad index"
    } else if status & SERVER_QUERY_NO_INDEX_USED != 0 {
        "No index"
    } else {
        return Ok(());
    };
    let mut msg = format!("{index} used in query/prepared statement ").into_bytes();
    msg.extend_from_slice(query);
    throw_sql_exception(vm, "00000", 0, &msg)
}

/// Converts a runtime error raised while converting values (e.g. a
/// `__toString()` that threw) back into a VM throw.
pub fn rt_throw(vm: &mut Vm, e: RuntimeError) -> Throw {
    if let Some(t) = vm.pending_throw.take() {
        return t;
    }
    match e {
        RuntimeError::Thrown(Value::Object(o)) => Throw::Exception(o),
        RuntimeError::Thrown(_) | RuntimeError::Fatal(_) => Throw::Fatal,
        RuntimeError::Custom { class, message } => vm.throw(&class.to_string_lossy(), &message),
        other => {
            let class = other.class_name().unwrap_or("Error");
            let msg = match &other {
                RuntimeError::Error(m)
                | RuntimeError::TypeError(m)
                | RuntimeError::ValueError(m)
                | RuntimeError::ArithmeticError(m)
                | RuntimeError::DivisionByZeroError(m)
                | RuntimeError::ArgumentCountError(m) => m.clone(),
                _ => String::new(),
            };
            vm.throw(class, &msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_flags() {
        assert_eq!(SERVER_QUERY_NO_GOOD_INDEX_USED, 16);
        assert_eq!(SERVER_QUERY_NO_INDEX_USED, 32);
        assert_eq!(REPORT_ERROR | REPORT_STRICT, 3);
    }
}

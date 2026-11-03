//! The error-handling core of `pdo_dbh.c`: `pdo_raise_impl_error()`,
//! `pdo_handle_error()`, `pdo_throw_exception()` and the argument-error
//! message builders (`zend_argument_*_error`).

use crate::consts::{ERRMODE_EXCEPTION, ERRMODE_SILENT, ERRMODE_WARNING};
use crate::dbh::Dbh;
use crate::host::{Ctx, PdoException};
use crate::stmt::Stmt;
use php_runtime::{RuntimeError, Value, ZArray};
use php_sqlite::pdo::sqlstate;

/// A `pdo_error_type`: five SQLSTATE bytes, all zero while unset.
pub(crate) type ErrCode = [u8; 5];

/// The unset error code (`error_code[0] == '\0'`).
pub(crate) const ERR_UNSET: ErrCode = [0; 5];
/// `PDO_ERR_NONE`.
pub(crate) const ERR_NONE: ErrCode = *b"00000";

/// The error code as text (`""` while unset).
pub(crate) fn code_str(code: &ErrCode) -> String {
    let end = code.iter().position(|&b| b == 0).unwrap_or(5);
    String::from_utf8_lossy(&code[..end]).into_owned()
}

/// Copies a SQLSTATE text into an error code (`strncpy`).
pub(crate) fn to_code(state: &str) -> ErrCode {
    let mut c = ERR_UNSET;
    for (d, s) in c.iter_mut().zip(state.bytes()) {
        *d = s;
    }
    c
}

/// `pdo_sqlstate_state_to_description()`.
fn describe(code: &ErrCode) -> &'static str {
    sqlstate::description(&code_str(code)).unwrap_or("<<Unknown error>>")
}

/// Builds the exception `pdo_throw_exception()` throws: message
/// `SQLSTATE[%s] [%d] %s`, integer code and a 3-element `errorInfo`.
pub fn exception_with_info(code: i64, message: &str, sqlstate: &str) -> PdoException {
    let info = ZArray::from_values([
        Value::from(sqlstate),
        Value::Int(code),
        Value::from(message),
    ]);
    PdoException {
        message: format!("SQLSTATE[{sqlstate}] [{code}] {message}"),
        code: Value::Int(code),
        error_info: Some(info),
    }
}

/// `pdo_throw_exception(driver_errcode, driver_errmsg, pdo_error)`.
pub fn throw_exception(cx: &mut Ctx<'_>, code: i64, message: &str, sqlstate: &str) {
    cx.throw_pdo(exception_with_info(code, message, sqlstate));
}

/// `pdo_raise_impl_error(dbh, stmt, sqlstate, supp)`: sets the error code
/// of the statement (or the handle) and reports the error according to
/// the error mode.
pub(crate) fn raise_impl_error(
    cx: &mut Ctx<'_>,
    dbh: &Dbh,
    stmt: Option<&Stmt>,
    sqlstate: &str,
    supp: Option<&str>,
) {
    let code = to_code(sqlstate);
    match stmt {
        Some(s) => s.error_code.set(code),
        None => dbh.error_code.set(code),
    }
    let mode = dbh.error_mode.get();
    if mode == ERRMODE_SILENT {
        return;
    }
    let state = code_str(&code);
    let msg = describe(&code);
    let message = match supp {
        Some(supp) => format!("SQLSTATE[{state}]: {msg}: {supp}"),
        None => format!("SQLSTATE[{state}]: {msg}"),
    };
    if mode == ERRMODE_EXCEPTION {
        let info = ZArray::from_values([Value::from(state.as_str()), Value::Int(0)]);
        cx.throw_pdo(PdoException {
            message,
            code: Value::from(state.as_str()),
            error_info: Some(info),
        });
    } else {
        cx.warning(&message);
    }
}

/// `pdo_handle_error(dbh, stmt)`: reports the error recorded in the
/// statement's (or the handle's) error code, with the driver's native
/// details.
pub(crate) fn handle_error(cx: &mut Ctx<'_>, dbh: &Dbh, stmt: Option<&Stmt>) {
    let mode = dbh.error_mode.get();
    if mode == ERRMODE_SILENT {
        return;
    }
    let code = match stmt {
        Some(s) => s.error_code.get(),
        None => dbh.error_code.get(),
    };
    let state = code_str(&code);
    let msg = describe(&code);
    let mut native_code = 0;
    let mut supp: Option<String> = None;
    let mut info: Option<ZArray> = None;
    if let Some(driver) = dbh.driver() {
        let stmt_driver = stmt.and_then(Stmt::driver);
        if let Some(extra) = driver.fetch_error(stmt_driver.as_deref()) {
            let mut arr = ZArray::from_values([Value::from(state.as_str())]);
            {
                let t = arr.make_mut();
                for v in extra {
                    let _ = t.push(v);
                }
            }
            if let Some(Value::Int(i)) = arr.get_int(1) {
                native_code = *i;
            }
            if let Some(v) = arr.get_int(2) {
                supp = Some(match v.as_str() {
                    Some(s) => s.to_string_lossy(),
                    None => String::new(),
                });
            }
            info = Some(arr);
        }
    }
    let message = match (&supp, native_code) {
        (Some(supp), n) if n != 0 => format!("SQLSTATE[{state}]: {msg}: {n} {supp}"),
        (Some(supp), _) => format!("SQLSTATE[{state}]: {msg}: {supp}"),
        (None, _) => format!("SQLSTATE[{state}]: {msg}"),
    };
    if mode == ERRMODE_WARNING {
        cx.warning(&message);
    } else if !cx.has_exception() {
        cx.throw_pdo(PdoException {
            message,
            code: Value::from(state.as_str()),
            error_info: info,
        });
    }
}

/// `zend_argument_error()`-style message: `"func(): Argument #n ($name) msg"`
/// (`name` is omitted for variadic positions).
pub(crate) fn arg_message(func: &str, n: u32, name: Option<&str>, msg: &str) -> String {
    match name {
        Some(name) => format!("{func}(): Argument #{n} (${name}) {msg}"),
        None => format!("{func}(): Argument #{n} {msg}"),
    }
}

/// A method being executed: its name and declared parameter names, used
/// to build argument errors.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Func {
    /// `Class::method`.
    pub name: &'static str,
    /// Declared parameter names (without the variadic one).
    pub args: &'static [&'static str],
}

impl Func {
    /// The parameter name of argument `n` (1-based).
    pub fn arg(&self, n: u32) -> Option<&'static str> {
        if n == 0 {
            return None;
        }
        self.args.get(n as usize - 1).copied()
    }

    /// `zend_argument_value_error(n, msg)`.
    pub fn value_error(&self, n: u32, msg: &str) -> RuntimeError {
        RuntimeError::ValueError(arg_message(self.name, n, self.arg(n), msg))
    }

    /// `zend_argument_type_error(n, msg)`.
    pub fn type_error(&self, n: u32, msg: &str) -> RuntimeError {
        RuntimeError::TypeError(arg_message(self.name, n, self.arg(n), msg))
    }

    /// `zend_argument_must_not_be_empty_error(n)`.
    pub fn empty_error(&self, n: u32) -> RuntimeError {
        self.value_error(n, "must not be empty")
    }

    /// `zend_argument_count_error(msg)` prefixed like
    /// `"%s() expects ..."`.
    pub fn count_error(&self, msg: &str) -> RuntimeError {
        RuntimeError::ArgumentCountError(format!("{}() {msg}", self.name))
    }

    /// `zend_wrong_parameter_type_error(n, expected, arg)`.
    pub fn wrong_type(&self, n: u32, expected: &str, given: &Value) -> RuntimeError {
        self.type_error(
            n,
            &format!("must be of type {expected}, {} given", given.value_name()),
        )
    }

    /// `zend_argument_error(pdo_exception_ce, n, msg)`: a `PDOException`.
    pub fn pdo_exception(&self, n: u32, msg: &str) -> PdoException {
        PdoException::new(arg_message(self.name, n, self.arg(n), msg), 0)
    }
}

macro_rules! funcs {
    ($($id:ident = $name:literal [$($arg:literal),*];)*) => {
        $(pub(crate) const $id: Func = Func { name: $name, args: &[$($arg),*] };)*
    };
}

funcs! {
    PDO_CONSTRUCT = "PDO::__construct" ["dsn", "username", "password", "options"];
    PDO_CONNECT = "PDO::connect" ["dsn", "username", "password", "options"];
    PDO_EXEC = "PDO::exec" ["statement"];
    PDO_PREPARE = "PDO::prepare" ["query", "options"];
    PDO_QUERY = "PDO::query" ["query", "fetchMode"];
    PDO_SET_ATTRIBUTE = "PDO::setAttribute" ["attribute", "value"];
    STMT_BIND_COLUMN = "PDOStatement::bindColumn" ["column", "var", "type", "maxLength", "driverOptions"];
    STMT_BIND_PARAM = "PDOStatement::bindParam" ["param", "var", "type", "maxLength", "driverOptions"];
    STMT_BIND_VALUE = "PDOStatement::bindValue" ["param", "value", "type"];
    STMT_FETCH = "PDOStatement::fetch" ["mode", "cursorOrientation", "cursorOffset"];
    STMT_FETCH_ALL = "PDOStatement::fetchAll" ["mode"];
    STMT_FETCH_OBJECT = "PDOStatement::fetchObject" ["class", "constructorArgs"];
    STMT_GET_COLUMN_META = "PDOStatement::getColumnMeta" ["column"];
    STMT_SET_FETCH_MODE = "PDOStatement::setFetchMode" ["mode"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(code_str(&ERR_UNSET), "");
        assert_eq!(code_str(&to_code("HY000")), "HY000");
        assert_eq!(describe(&to_code("HY093")), "Invalid parameter number");
        assert_eq!(describe(&to_code("ZZZZZ")), "<<Unknown error>>");
    }

    #[test]
    fn argument_messages() {
        assert_eq!(
            PDO_CONSTRUCT.value_error(4, "x").message(),
            Some("PDO::__construct(): Argument #4 ($options) x")
        );
        assert_eq!(
            STMT_SET_FETCH_MODE.type_error(2, "y").message(),
            Some("PDOStatement::setFetchMode(): Argument #2 y")
        );
    }

    #[test]
    fn throw_exception_info() {
        let e = exception_with_info(14, "unable to open", "HY000");
        assert_eq!(e.message, "SQLSTATE[HY000] [14] unable to open");
        assert_eq!(e.error_info.map(|a| a.len()), Some(3));
    }
}

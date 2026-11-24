// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `mysql` driver (`ext/pdo_mysql`) over the pure-Rust mysqlnd client
//! of `php-mysqlnd`.
//!
//! References: `ext/pdo_mysql/mysql_driver.c` (handle factory, handle
//! methods, error mapping), `ext/pdo_mysql/mysql_statement.c` (statement
//! methods), `ext/pdo_mysql/mysql_sql_parser.re` (placeholder scanner),
//! `ext/pdo_mysql/pdo_mysql.c` (constants, `Pdo\Mysql`).
//!
//! As with a mysqlnd build of PHP, ints and floats are fetched natively
//! unless `PDO::ATTR_STRINGIFY_FETCHES` is set
//! (`MYSQLND_OPT_INT_AND_FLOAT_NATIVE`), emulated prepares are the default
//! and native prepares use the binary protocol.

use crate::consts::{
    param_type, ATTR_AUTOCOMMIT, ATTR_CLIENT_VERSION, ATTR_CONNECTION_STATUS,
    ATTR_DEFAULT_STR_PARAM, ATTR_DRIVER_SPECIFIC, ATTR_EMULATE_PREPARES, ATTR_FETCH_TABLE_NAMES,
    ATTR_SERVER_INFO, ATTR_SERVER_VERSION, ATTR_STRINGIFY_FETCHES, ATTR_TIMEOUT, CASE_NATURAL,
    PARAM_INT, PARAM_LOB, PARAM_NULL, PARAM_STMT, PARAM_STR, PARAM_STR_CHAR, PARAM_STR_NATL,
};
use crate::driver::{
    BoundParam, ColumnData, ConnectParams, Connection, DbhDriver, DbhFlags, DriverEntry,
    DriverError, ExecuteInfo, ParamEvent, Prepared, StmtDriver,
};
use crate::host::{Ctx, DriverCx};
use php_mysqlnd::consts::opt::{
    MYSQLND_OPT_INT_AND_FLOAT_NATIVE, MYSQL_INIT_COMMAND, MYSQL_OPT_COMPRESS,
    MYSQL_OPT_CONNECT_TIMEOUT, MYSQL_OPT_LOAD_DATA_LOCAL_DIR, MYSQL_OPT_LOCAL_INFILE,
    MYSQL_SERVER_PUBLIC_KEY, MYSQL_SET_CHARSET_NAME,
};
use php_mysqlnd::consts::{
    BLOB_FLAG, CLIENT_FOUND_ROWS, CLIENT_IGNORE_SPACE, CLIENT_MULTI_RESULTS,
    CLIENT_MULTI_STATEMENTS, CLIENT_SSL_DONT_VERIFY_SERVER_CERT, CLIENT_SSL_VERIFY_SERVER_CERT,
    MULTIPLE_KEY_FLAG, MYSQL_TYPE_BIT, MYSQL_TYPE_BLOB, MYSQL_TYPE_DATE, MYSQL_TYPE_DATETIME,
    MYSQL_TYPE_DECIMAL, MYSQL_TYPE_DOUBLE, MYSQL_TYPE_ENUM, MYSQL_TYPE_FLOAT, MYSQL_TYPE_GEOMETRY,
    MYSQL_TYPE_INT24, MYSQL_TYPE_JSON, MYSQL_TYPE_LONG, MYSQL_TYPE_LONGLONG, MYSQL_TYPE_LONG_BLOB,
    MYSQL_TYPE_MEDIUM_BLOB, MYSQL_TYPE_NEWDATE, MYSQL_TYPE_NEWDECIMAL, MYSQL_TYPE_NULL,
    MYSQL_TYPE_SET, MYSQL_TYPE_SHORT, MYSQL_TYPE_STRING, MYSQL_TYPE_TIME, MYSQL_TYPE_TIMESTAMP,
    MYSQL_TYPE_TINY, MYSQL_TYPE_TINY_BLOB, MYSQL_TYPE_VAR_STRING, MYSQL_TYPE_VECTOR,
    MYSQL_TYPE_YEAR, NOT_NULL_FLAG, PRI_KEY_FLAG, SERVER_STATUS_IN_TRANS, UNIQUE_KEY_FLAG,
};
use php_mysqlnd::{CloseType, ExecuteError, Metadata, OptionValue, ResultSet, Statement};
use php_runtime::{
    ArrayKey, CastTarget, ErrorLevel, HashTable, Obj, RtResult, RuntimeContext, Value, ZArray, ZStr,
};
use php_sqlite::pdo::parser::{
    parse_params, ParseContext, ParseError, Parsed, Scanner, Token, PLACEHOLDER_NONE,
    PLACEHOLDER_POSITIONAL,
};
use php_sqlite::pdo::SqlState;
use php_sqlite::ExceptionClass;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

/// `PDO_MYSQL_ATTR_USE_BUFFERED_QUERY`.
pub const ATTR_USE_BUFFERED_QUERY: i64 = ATTR_DRIVER_SPECIFIC;
/// `PDO_MYSQL_ATTR_LOCAL_INFILE`.
pub const ATTR_LOCAL_INFILE: i64 = ATTR_DRIVER_SPECIFIC + 1;
/// `PDO_MYSQL_ATTR_INIT_COMMAND`.
pub const ATTR_INIT_COMMAND: i64 = ATTR_DRIVER_SPECIFIC + 2;
/// `PDO_MYSQL_ATTR_COMPRESS`.
pub const ATTR_COMPRESS: i64 = ATTR_DRIVER_SPECIFIC + 3;
/// `PDO_MYSQL_ATTR_FOUND_ROWS`.
pub const ATTR_FOUND_ROWS: i64 = ATTR_DRIVER_SPECIFIC + 4;
/// `PDO_MYSQL_ATTR_IGNORE_SPACE`.
pub const ATTR_IGNORE_SPACE: i64 = ATTR_DRIVER_SPECIFIC + 5;
/// `PDO_MYSQL_ATTR_SSL_KEY`.
pub const ATTR_SSL_KEY: i64 = ATTR_DRIVER_SPECIFIC + 6;
/// `PDO_MYSQL_ATTR_SSL_CERT`.
pub const ATTR_SSL_CERT: i64 = ATTR_DRIVER_SPECIFIC + 7;
/// `PDO_MYSQL_ATTR_SSL_CA`.
pub const ATTR_SSL_CA: i64 = ATTR_DRIVER_SPECIFIC + 8;
/// `PDO_MYSQL_ATTR_SSL_CAPATH`.
pub const ATTR_SSL_CAPATH: i64 = ATTR_DRIVER_SPECIFIC + 9;
/// `PDO_MYSQL_ATTR_SSL_CIPHER`.
pub const ATTR_SSL_CIPHER: i64 = ATTR_DRIVER_SPECIFIC + 10;
/// `PDO_MYSQL_ATTR_SERVER_PUBLIC_KEY`.
pub const ATTR_SERVER_PUBLIC_KEY: i64 = ATTR_DRIVER_SPECIFIC + 11;
/// `PDO_MYSQL_ATTR_MULTI_STATEMENTS`.
pub const ATTR_MULTI_STATEMENTS: i64 = ATTR_DRIVER_SPECIFIC + 12;
/// `PDO_MYSQL_ATTR_SSL_VERIFY_SERVER_CERT`.
pub const ATTR_SSL_VERIFY_SERVER_CERT: i64 = ATTR_DRIVER_SPECIFIC + 13;
/// `PDO_MYSQL_ATTR_LOCAL_INFILE_DIRECTORY`.
pub const ATTR_LOCAL_INFILE_DIRECTORY: i64 = ATTR_DRIVER_SPECIFIC + 14;

/// The `Pdo\Mysql` class constants, in declaration order
/// (`pdo_mysql.stub.php`, mysqlnd build). Each is also a deprecated
/// `PDO::MYSQL_<name>` alias (since 8.5).
pub const PDO_MYSQL_CLASS_CONSTANTS: &[(&str, i64)] = &[
    ("ATTR_USE_BUFFERED_QUERY", ATTR_USE_BUFFERED_QUERY),
    ("ATTR_LOCAL_INFILE", ATTR_LOCAL_INFILE),
    ("ATTR_INIT_COMMAND", ATTR_INIT_COMMAND),
    ("ATTR_COMPRESS", ATTR_COMPRESS),
    ("ATTR_DIRECT_QUERY", ATTR_EMULATE_PREPARES),
    ("ATTR_FOUND_ROWS", ATTR_FOUND_ROWS),
    ("ATTR_IGNORE_SPACE", ATTR_IGNORE_SPACE),
    ("ATTR_SSL_KEY", ATTR_SSL_KEY),
    ("ATTR_SSL_CERT", ATTR_SSL_CERT),
    ("ATTR_SSL_CA", ATTR_SSL_CA),
    ("ATTR_SSL_CAPATH", ATTR_SSL_CAPATH),
    ("ATTR_SSL_CIPHER", ATTR_SSL_CIPHER),
    ("ATTR_SERVER_PUBLIC_KEY", ATTR_SERVER_PUBLIC_KEY),
    ("ATTR_MULTI_STATEMENTS", ATTR_MULTI_STATEMENTS),
    ("ATTR_SSL_VERIFY_SERVER_CERT", ATTR_SSL_VERIFY_SERVER_CERT),
    ("ATTR_LOCAL_INFILE_DIRECTORY", ATTR_LOCAL_INFILE_DIRECTORY),
];

/// `mysql_get_client_info()` of mysqlnd (`phpinfo()`, `ATTR_CLIENT_VERSION`).
pub use php_mysqlnd::CLIENT_INFO;

/// `PDO_MYSQL_UNIX_ADDR`: the compiled-in default of
/// `pdo_mysql.default_socket` (Debian/Ubuntu layout, as `/usr/bin/php8.5`).
pub const DEFAULT_SOCKET: &str = "/var/run/mysqld/mysqld.sock";

/// The `pdo_mysql` driver entry.
pub const MYSQL_DRIVER: DriverEntry = DriverEntry {
    name: "mysql",
    specific_class: Some("Pdo\\Mysql"),
    factory: connect,
};

const CR_COMMANDS_OUT_OF_SYNC: u32 = 2014;
const CR_NEW_STMT_METADATA: u32 = 2057;
const ER_UNSUPPORTED_PS: u32 = 1295;

const MSG_PENDING_RESULT_SETS: &str = "Cannot execute queries while there are pending result sets. Consider unsetting the previous PDOStatement or calling PDOStatement::closeCursor()";
const MSG_UNBUFFERED_ACTIVE: &str = "Cannot execute queries while other unbuffered queries are active.  Consider using PDOStatement::fetchAll().  Alternatively, if your code is only ever going to run against mysql, you may enable query buffering by setting the Pdo\\Mysql::ATTR_USE_BUFFERED_QUERY attribute.";
const MSG_NEW_STMT_METADATA: &str = "A stored procedure returning result sets of different size was called. This is not supported by libmysql";

// ---------------------------------------------------------------------------
// DSN
// ---------------------------------------------------------------------------

/// The connection parameters `pdo_mysql_handle_factory()` extracts from the
/// data source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnParts {
    /// `charset`.
    pub charset: Option<Vec<u8>>,
    /// `dbname` (default empty).
    pub dbname: Vec<u8>,
    /// `host` (default `localhost`).
    pub host: Vec<u8>,
    /// `port` (`atoi`, default 3306).
    pub port: u32,
    /// The Unix socket, used only when the host is `localhost`.
    pub unix_socket: Option<Vec<u8>>,
    /// `user`.
    pub user: Option<Vec<u8>>,
    /// `password`.
    pub password: Option<Vec<u8>>,
}

/// C `atoi()`: leading whitespace, optional sign, digits.
fn atoi(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || (b'\t'..=b'\r').contains(&s[i])) {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut n: i64 = 0;
    while let Some(d) = s.get(i).filter(|c| c.is_ascii_digit()) {
        n = n.wrapping_mul(10).wrapping_add(i64::from(d - b'0'));
        i += 1;
    }
    if neg {
        n.wrapping_neg()
    } else {
        n
    }
}

/// Parses a `mysql:` data source like `pdo_mysql_handle_factory()`.
pub fn parse_dsn(data_source: &[u8], default_socket: Option<&[u8]>) -> DsnParts {
    let mut vars: [(&str, Option<Vec<u8>>); 7] = [
        ("charset", None),
        ("dbname", Some(Vec::new())),
        ("host", Some(b"localhost".to_vec())),
        ("port", Some(b"3306".to_vec())),
        ("unix_socket", default_socket.map(<[u8]>::to_vec)),
        ("user", None),
        ("password", None),
    ];
    crate::dsn::parse_data_source(data_source, &mut vars);
    let [charset, dbname, host, port, socket, user, password] = vars.map(|(_, v)| v);
    let host = host.unwrap_or_default();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let port = port.map_or(3306, |p| atoi(&p) as u32);
    let unix_socket = if host == b"localhost" { socket } else { None };
    DsnParts {
        charset,
        dbname: dbname.unwrap_or_default(),
        host,
        port,
        unix_socket,
        user,
        password,
    }
}

// ---------------------------------------------------------------------------
// the scanner (mysql_sql_parser.re)
// ---------------------------------------------------------------------------

/// The `pdo_mysql_scanner`: backslash escapes inside quoted strings,
/// backtick identifiers and `#` / `-- ` comments.
#[derive(Debug, Clone, Copy, Default)]
pub struct MysqlScanner;

/// Byte at `i`, with the terminating NUL past the end.
fn at(input: &[u8], i: usize) -> u8 {
    input.get(i).copied().unwrap_or(0)
}

/// `([q]((qq)|([\\]ANYNOEOF)|ANYNOEOF\[q\\])*[q])` (`backslash`) or
/// `([q]([q][q]|ANYNOEOF\[q])*[q])`: the longest quoted run.
fn quoted(input: &[u8], pos: usize, q: u8, backslash: bool) -> Option<usize> {
    if at(input, pos) != q {
        return None;
    }
    let mut best = None;
    let mut i = pos + 1;
    loop {
        match at(input, i) {
            0 => return best,
            b'\\' if backslash => {
                if at(input, i + 1) == 0 {
                    return best;
                }
                i += 2;
            }
            c if c == q => {
                best = Some(i + 1);
                if at(input, i + 1) == q {
                    i += 2;
                } else {
                    return best;
                }
            }
            _ => i += 1,
        }
    }
}

/// `COMMENTS`; `Err(())` when the comment runs into the end of the buffer
/// (`YYFILL` returns `PDO_PARSER_EOI`).
fn mysql_comment(input: &[u8], pos: usize) -> Option<Result<usize, ()>> {
    let line = |start: usize| {
        let mut i = start;
        while i <= input.len() {
            if at(input, i) == b'\n' {
                return Ok(i);
            }
            i += 1;
        }
        Err(())
    };
    match (at(input, pos), at(input, pos + 1)) {
        (b'-', b'-') if matches!(at(input, pos + 2), b' ' | b'\t' | 0x0b | 0x0c | b'\r') => {
            Some(line(pos + 3))
        }
        (b'#', _) if pos < input.len() => Some(line(pos + 1)),
        (b'/', b'*') => {
            let mut i = pos + 2;
            while i < input.len() {
                if input[i] == b'*' {
                    let mut j = i;
                    while j < input.len() && input[j] == b'*' {
                        j += 1;
                    }
                    if j < input.len() && input[j] == b'/' {
                        return Some(Ok(j + 1));
                    }
                    i = j;
                    continue;
                }
                i += 1;
            }
            Some(Err(()))
        }
        _ => None,
    }
}

const MYSQL_SPECIALS: &[u8] = b":?\"'`/#-";

impl Scanner for MysqlScanner {
    fn scan(&self, input: &[u8], pos: usize) -> (Token, usize) {
        if pos >= input.len() {
            return (Token::Eoi, pos);
        }
        let c = input[pos];
        let multichar = if c == b':' || c == b'?' {
            let mut i = pos;
            while at(input, i) == c {
                i += 1;
            }
            (i - pos >= 2).then_some(i)
        } else {
            None
        };
        let bindchr = if c == b':' {
            let mut i = pos + 1;
            while at(input, i).is_ascii_alphanumeric() || at(input, i) == b'_' {
                i += 1;
            }
            (i > pos + 1).then_some(i)
        } else {
            None
        };
        let comment = match mysql_comment(input, pos) {
            Some(Err(())) => return (Token::Eoi, input.len()),
            Some(Ok(end)) => Some(end),
            None => None,
        };
        let mut run = pos;
        while run < input.len() && input[run] != 0 && !MYSQL_SPECIALS.contains(&input[run]) {
            run += 1;
        }
        // re2c rule order: ties go to the earlier rule.
        let candidates = [
            (quoted(input, pos, b'"', true), Token::Text),
            (quoted(input, pos, b'\'', true), Token::Text),
            (quoted(input, pos, b'`', false), Token::Text),
            (multichar, Token::Text),
            (bindchr, Token::Bind),
            ((c == b'?').then_some(pos + 1), Token::BindPos),
            (MYSQL_SPECIALS.contains(&c).then_some(pos + 1), Token::Text),
            (comment, Token::Text),
            ((run > pos).then_some(run), Token::Text),
        ];
        let mut best: Option<(Token, usize)> = None;
        for (end, tok) in candidates {
            if let Some(end) = end {
                if end > pos && best.is_none_or(|(_, b)| end > b) {
                    best = Some((tok, end));
                }
            }
        }
        // An embedded NUL matches no rule; treat it as one byte of text.
        best.unwrap_or((Token::Text, pos + 1))
    }
}

// ---------------------------------------------------------------------------
// quoting
// ---------------------------------------------------------------------------

/// The quoting style `mysql_handle_quoter()` picks for a parameter type.
fn quote_prefix(param_type: i64, assume_national: bool) -> &'static [u8] {
    if param_type & PARAM_LOB == PARAM_LOB {
        return b"_binary'";
    }
    let mut national = assume_national;
    if param_type & PARAM_STR_NATL == PARAM_STR_NATL {
        national = true;
    }
    if param_type & PARAM_STR_CHAR == PARAM_STR_CHAR {
        national = false;
    }
    if national {
        b"N'"
    } else {
        b"'"
    }
}

/// Wraps an already escaped string: `'...'`, `N'...'` or `_binary'...'`.
pub fn wrap_quoted(escaped: &[u8], param_type: i64, assume_national: bool) -> Vec<u8> {
    let prefix = quote_prefix(param_type, assume_national);
    let mut out = Vec::with_capacity(prefix.len() + escaped.len() + 1);
    out.extend_from_slice(prefix);
    out.extend_from_slice(escaped);
    out.push(b'\'');
    out
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// `pdo_mysql_error_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ErrorInfo {
    errcode: u32,
    errmsg: Option<Vec<u8>>,
}

fn sqlstate_of(s: &str) -> SqlState {
    let mut b = *b"00000";
    for (d, c) in b.iter_mut().zip(s.bytes()) {
        *d = c;
    }
    SqlState(b)
}

/// The message `_pdo_mysql_error()` records for an error number.
fn error_message(errno: u32, more_results: bool, native: Vec<u8>) -> Option<Vec<u8>> {
    match errno {
        0 => None,
        CR_COMMANDS_OUT_OF_SYNC if more_results => {
            Some(MSG_PENDING_RESULT_SETS.as_bytes().to_vec())
        }
        CR_COMMANDS_OUT_OF_SYNC => Some(MSG_UNBUFFERED_ACTIVE.as_bytes().to_vec()),
        CR_NEW_STMT_METADATA => Some(MSG_NEW_STMT_METADATA.as_bytes().to_vec()),
        _ => Some(native),
    }
}

/// Emits the warnings mysqlnd queued while running a driver call.
fn flush_diag(cx: &mut DriverCx<'_>) {
    for d in php_mysqlnd::diag::take() {
        let level = if d.fatal {
            ErrorLevel::Error
        } else {
            ErrorLevel::Warning
        };
        if d.docref {
            cx.docref(level, &d.message);
        } else {
            cx.error(level, &d.message);
        }
    }
}

fn flush_diag_ctx(cx: &mut Ctx<'_>) {
    for d in php_mysqlnd::diag::take() {
        let level = if d.fatal {
            ErrorLevel::Error
        } else {
            ErrorLevel::Warning
        };
        if d.docref {
            cx.docref(level, &d.message);
        } else {
            cx.error(level, &d.message);
        }
    }
}

// ---------------------------------------------------------------------------
// the handle
// ---------------------------------------------------------------------------

/// `pdo_mysql_db_handle`.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
struct Handle {
    server: php_mysqlnd::Connection,
    einfo: RefCell<ErrorInfo>,
    auto_commit: Cell<bool>,
    buffered: Cell<bool>,
    emulate_prepare: Cell<bool>,
    fetch_table_names: Cell<bool>,
    assume_national: Cell<bool>,
    local_infile: Cell<bool>,
    local_infile_directory: Option<Vec<u8>>,
}

impl Handle {
    /// `pdo_mysql_error(dbh)`.
    fn error(&self) -> DriverError {
        let errno = self.server.errno();
        let msg = error_message(errno, self.server.more_results(), self.server.error());
        *self.einfo.borrow_mut() = ErrorInfo {
            errcode: errno,
            errmsg: msg,
        };
        DriverError::State(sqlstate_of(&self.server.sqlstate()))
    }

    /// `mysql_handle_doer()`: `Ok(-1)` never happens; failures are `Err`.
    fn doer(&self, sql: &[u8]) -> Result<i64, DriverError> {
        if self.server.query(sql).is_err() {
            return Err(self.error());
        }
        let c = self.server.affected_rows();
        if c == u64::MAX {
            let e = self.error();
            return if self.einfo.borrow().errcode != 0 {
                Err(e)
            } else {
                Ok(0)
            };
        }
        while self.server.more_results() {
            match self.server.next_result() {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => return Err(self.error()),
            }
            if let Ok(Some(mut r)) = self.server.store_result() {
                r.free(false);
            }
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let n = i64::from(c as i32);
        Ok(n)
    }

    /// Eats every pending result set (`MULTI_QUERY` leftovers).
    fn drain_results(&self) -> bool {
        while self.server.more_results() {
            match self.server.next_result() {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => return false,
            }
            if let Ok(Some(mut r)) = self.server.store_result() {
                r.free(false);
            }
        }
        true
    }
}

/// A `MySQL` connection.
#[derive(Debug)]
pub struct MysqlDbh {
    h: Rc<Handle>,
}

impl MysqlDbh {
    /// The mysqlnd connection (for `Pdo\Mysql::getWarningCount()`).
    pub fn server(&self) -> &php_mysqlnd::Connection {
        &self.h.server
    }

    /// `mysql_warning_count()`.
    pub fn warning_count(&self) -> i64 {
        i64::from(self.h.server.warning_count())
    }
}

/// `pdo_attr_lval()` over the core context.
fn attr_lval(cx: &mut Ctx<'_>, options: Option<&ZArray>, option: i64, default: i64) -> i64 {
    match options.and_then(|o| o.get_int(option)) {
        Some(v) => {
            let r = v.deref().to_long(cx.host());
            cx.check(r).unwrap_or(0)
        }
        None => default,
    }
}

/// `pdo_attr_strval()`.
fn attr_strval(cx: &mut Ctx<'_>, options: Option<&ZArray>, option: i64) -> Option<Vec<u8>> {
    let v = options.and_then(|o| o.get_int(option))?.deref();
    let r = v.to_zstr(cx.host());
    cx.check(r).map(|s| s.as_bytes().to_vec())
}

/// `pdo_mysql_error(dbh)` while connecting (`dbh->methods` unset): throws.
fn throw_connect_error(cx: &mut Ctx<'_>, server: &php_mysqlnd::Connection) {
    flush_diag_ctx(cx);
    let errno = server.errno();
    let msg = error_message(errno, server.more_results(), server.error()).unwrap_or_default();
    let state = sqlstate_of(&server.sqlstate());
    crate::error::throw_exception(
        cx,
        i64::from(errno),
        &String::from_utf8_lossy(&msg),
        state.as_str(),
    );
}

fn lossy(v: Option<&Vec<u8>>) -> Option<String> {
    v.map(|b| String::from_utf8_lossy(b).into_owned())
}

/// `pdo_mysql_handle_factory()`.
#[allow(clippy::too_many_lines)]
fn connect(
    cx: &mut Ctx<'_>,
    params: &ConnectParams,
    options: Option<&ZArray>,
) -> Option<Connection> {
    let default_socket = cx
        .host()
        .cfg_get_string(b"pdo_mysql.default_socket")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SOCKET.as_bytes().to_vec());
    let dsn = parse_dsn(&params.data_source, Some(default_socket.as_slice()));

    let mut connect_opts = CLIENT_MULTI_RESULTS;
    if options.is_none() || attr_lval(cx, options, ATTR_MULTI_STATEMENTS, 1) != 0 {
        connect_opts |= CLIENT_MULTI_STATEMENTS;
    }
    let server = php_mysqlnd::Connection::init_ex(params.is_persistent);
    if params.is_persistent {
        server.restart_psession();
    }
    let mut buffered = true;
    let mut emulate = true;
    let mut assume_national = false;
    let mut local_infile = false;
    let mut local_infile_directory = None;
    if options.is_some() {
        let timeout = attr_lval(cx, options, ATTR_TIMEOUT, 30);
        buffered = attr_lval(cx, options, ATTR_USE_BUFFERED_QUERY, 1) != 0;
        emulate = attr_lval(cx, options, ATTR_EMULATE_PREPARES, 1) != 0;
        assume_national = attr_lval(cx, options, ATTR_DEFAULT_STR_PARAM, 0) == PARAM_STR_NATL;
        if attr_lval(cx, options, ATTR_FOUND_ROWS, 0) != 0 {
            connect_opts |= CLIENT_FOUND_ROWS;
        }
        if attr_lval(cx, options, ATTR_IGNORE_SPACE, 0) != 0 {
            connect_opts |= CLIENT_IGNORE_SPACE;
        }
        if !server.set_option(MYSQL_OPT_CONNECT_TIMEOUT, OptionValue::Int(timeout)) {
            throw_connect_error(cx, &server);
            return None;
        }
        if attr_lval(cx, options, ATTR_LOCAL_INFILE, 0) != 0 {
            local_infile = true;
        }
        if let Some(dir) = attr_strval(cx, options, ATTR_LOCAL_INFILE_DIRECTORY) {
            if !cx.host().check_open_basedir(&dir) {
                if !server.set_option(MYSQL_OPT_LOAD_DATA_LOCAL_DIR, OptionValue::Str(&dir)) {
                    throw_connect_error(cx, &server);
                    return None;
                }
                // mysqlnd stores an empty directory as NULL
                // (`MYSQL_OPT_LOAD_DATA_LOCAL_DIR`), which getAttribute() reads
                local_infile_directory = Some(dir).filter(|d| !d.is_empty());
            }
        }
        if let Some(cmd) = attr_strval(cx, options, ATTR_INIT_COMMAND) {
            if !server.set_option(MYSQL_INIT_COMMAND, OptionValue::Str(&cmd)) {
                throw_connect_error(cx, &server);
                return None;
            }
        }
        if attr_lval(cx, options, ATTR_COMPRESS, 0) != 0
            && !server.set_option(MYSQL_OPT_COMPRESS, OptionValue::Int(0))
        {
            throw_connect_error(cx, &server);
            return None;
        }
        let key = attr_strval(cx, options, ATTR_SSL_KEY);
        let cert = attr_strval(cx, options, ATTR_SSL_CERT);
        let ca = attr_strval(cx, options, ATTR_SSL_CA);
        let capath = attr_strval(cx, options, ATTR_SSL_CAPATH);
        let cipher = attr_strval(cx, options, ATTR_SSL_CIPHER);
        if key.is_some() || cert.is_some() || ca.is_some() || capath.is_some() || cipher.is_some() {
            let (key, cert, ca, capath, cipher) = (
                lossy(key.as_ref()),
                lossy(cert.as_ref()),
                lossy(ca.as_ref()),
                lossy(capath.as_ref()),
                lossy(cipher.as_ref()),
            );
            server.ssl_set(
                key.as_deref(),
                cert.as_deref(),
                ca.as_deref(),
                capath.as_deref(),
                cipher.as_deref(),
            );
        }
        if let Some(pk) = attr_strval(cx, options, ATTR_SERVER_PUBLIC_KEY) {
            if !server.set_option(MYSQL_SERVER_PUBLIC_KEY, OptionValue::Str(&pk)) {
                throw_connect_error(cx, &server);
                return None;
            }
        }
        let verify = attr_lval(cx, options, ATTR_SSL_VERIFY_SERVER_CERT, -1);
        if verify != -1 {
            connect_opts |= if verify != 0 {
                CLIENT_SSL_VERIFY_SERVER_CERT
            } else {
                CLIENT_SSL_DONT_VERIFY_SERVER_CERT
            };
        }
    }
    if cx.has_exception() {
        return None;
    }
    if !server.set_option(
        MYSQL_OPT_LOCAL_INFILE,
        OptionValue::Int(i64::from(local_infile)),
    ) {
        throw_connect_error(cx, &server);
        return None;
    }
    let native = attr_lval(
        cx,
        options,
        ATTR_STRINGIFY_FETCHES,
        i64::from(params.stringify),
    ) == 0;
    if !server.set_option(
        MYSQLND_OPT_INT_AND_FLOAT_NATIVE,
        OptionValue::Int(i64::from(native)),
    ) {
        throw_connect_error(cx, &server);
        return None;
    }
    if let Some(cs) = &dsn.charset {
        if !server.set_option(MYSQL_SET_CHARSET_NAME, OptionValue::Str(cs)) {
            throw_connect_error(cx, &server);
            return None;
        }
    }
    let username = params.username.clone().or_else(|| dsn.user.clone());
    let password = params.password.clone().or_else(|| dsn.password.clone());
    let host = String::from_utf8_lossy(&dsn.host).into_owned();
    let socket = lossy(dsn.unix_socket.as_ref());
    let db = (!dsn.dbname.is_empty()).then_some(dsn.dbname.as_slice());
    let r = server.connect(
        Some(host.as_str()),
        username.as_deref(),
        password.as_deref(),
        db,
        dsn.port,
        socket.as_deref(),
        connect_opts,
    );
    if r.is_err() {
        throw_connect_error(cx, &server);
        return None;
    }
    flush_diag_ctx(cx);
    if !params.auto_commit && server.autocommit(false).is_err() {
        throw_connect_error(cx, &server);
        return None;
    }
    let h = Rc::new(Handle {
        server,
        einfo: RefCell::new(ErrorInfo::default()),
        auto_commit: Cell::new(params.auto_commit),
        buffered: Cell::new(buffered),
        emulate_prepare: Cell::new(emulate),
        fetch_table_names: Cell::new(false),
        assume_national: Cell::new(assume_national),
        local_infile: Cell::new(local_infile),
        local_infile_directory,
    });
    Some(Connection {
        driver: Rc::new(MysqlDbh { h }),
        flags: DbhFlags {
            alloc_own_columns: true,
            max_escaped_char_length: 2,
            skip_param_evt: ParamEvent::Free.bit()
                | ParamEvent::ExecPost.bit()
                | ParamEvent::FetchPre.bit()
                | ParamEvent::FetchPost.bit()
                | ParamEvent::Normalize.bit(),
            native_case: CASE_NATURAL,
        },
    })
}

/// `pdo_get_bool_param()`.
fn get_bool_param(cx: &mut DriverCx<'_>, value: &Value) -> Option<bool> {
    match value.deref() {
        Value::Bool(b) => Some(b),
        Value::Int(i) => Some(i != 0),
        _ => {
            cx.throw_new(
                ExceptionClass::TypeError,
                format!(
                    "Attribute value must be of type bool for selected attribute, {} given",
                    value.value_name()
                ),
                0,
            );
            None
        }
    }
}

impl DbhDriver for MysqlDbh {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn close(&self, _cx: &mut DriverCx<'_>) {
        self.h.server.close(CloseType::Explicit);
    }

    fn prepare(
        &self,
        cx: &mut DriverCx<'_>,
        sql: &ZStr,
        _options: Option<&ZArray>,
    ) -> Result<Prepared, DriverError> {
        let emulated = |h: &Rc<Handle>| Prepared {
            stmt: Rc::new(MysqlStmt::new(h.clone(), None, 0)),
            supports_placeholders: PLACEHOLDER_NONE,
            named_rewrite_template: None,
            bound_param_map: None,
        };
        if self.h.emulate_prepare.get() || self.h.server.server_version() < 40100 {
            return Ok(emulated(&self.h));
        }
        let mut map = Vec::new();
        let parsed = {
            let mut pctx = ParseContext {
                supports_placeholders: PLACEHOLDER_POSITIONAL,
                named_rewrite_template: None,
                bound_params: None,
                bound_param_map: &mut map,
            };
            parse_params(cx, &MysqlScanner, sql.as_bytes(), &mut pctx, None)
        };
        let query = match parsed {
            Ok(Parsed::Unchanged) => sql.as_bytes().to_vec(),
            Ok(Parsed::Rewritten(q)) => q,
            Err(ParseError::Impl { sqlstate, message }) => {
                return Err(DriverError::Impl(sqlstate, message.to_string()));
            }
            Err(_) => return Err(DriverError::Silent),
        };
        let st = self.h.server.stmt_init();
        let r = st.prepare(&query);
        flush_diag(cx);
        if r.is_err() {
            if self.h.server.errno() == ER_UNSUPPORTED_PS {
                let _ = st.close();
                return Ok(emulated(&self.h));
            }
            return Err(self.h.error());
        }
        let num_params = st.param_count();
        Ok(Prepared {
            stmt: Rc::new(MysqlStmt::new(self.h.clone(), Some(st), num_params)),
            supports_placeholders: PLACEHOLDER_POSITIONAL,
            named_rewrite_template: None,
            bound_param_map: (!map.is_empty()).then_some(map),
        })
    }

    fn exec(&self, cx: &mut DriverCx<'_>, sql: &ZStr) -> Result<i64, DriverError> {
        let r = self.h.doer(sql.as_bytes());
        flush_diag(cx);
        r
    }

    fn quote(
        &self,
        _cx: &mut DriverCx<'_>,
        unquoted: &[u8],
        param_type: i64,
        _error_mode: i64,
    ) -> Option<Result<ZStr, DriverError>> {
        let escaped = self.h.server.escape_string(unquoted);
        Some(Ok(ZStr::from(wrap_quoted(
            &escaped,
            param_type,
            self.h.assume_national.get(),
        ))))
    }

    fn begin(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let r = self.h.doer(b"START TRANSACTION").map(|_| ());
        flush_diag(cx);
        Some(r)
    }

    fn commit(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let r = self.h.server.commit(0, None).map_err(|_| self.h.error());
        flush_diag(cx);
        Some(r)
    }

    fn rollback(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        let r = self.h.server.rollback(0, None).map_err(|_| self.h.error());
        flush_diag(cx);
        Some(r)
    }

    fn set_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
        value: &Value,
    ) -> Option<Result<(), DriverError>> {
        let h = &self.h;
        let r = match attr {
            ATTR_AUTOCOMMIT => {
                let Some(b) = get_bool_param(cx, value) else {
                    return Some(Err(DriverError::Silent));
                };
                if h.auto_commit.get() != b {
                    h.auto_commit.set(b);
                    if h.server.autocommit(b).is_err() {
                        let e = h.error();
                        flush_diag(cx);
                        return Some(Err(e));
                    }
                }
                Ok(())
            }
            ATTR_DEFAULT_STR_PARAM => {
                let Some(l) = php_sqlite::pdo::get_long_param(cx, value) else {
                    return Some(Err(DriverError::Silent));
                };
                h.assume_national.set(l == PARAM_STR_NATL);
                Ok(())
            }
            ATTR_USE_BUFFERED_QUERY | ATTR_EMULATE_PREPARES | ATTR_FETCH_TABLE_NAMES => {
                let Some(b) = get_bool_param(cx, value) else {
                    return Some(Err(DriverError::Silent));
                };
                match attr {
                    ATTR_USE_BUFFERED_QUERY => h.buffered.set(b),
                    ATTR_EMULATE_PREPARES => h.emulate_prepare.set(b),
                    _ => h.fetch_table_names.set(b),
                }
                Ok(())
            }
            ATTR_STRINGIFY_FETCHES => {
                let Some(b) = get_bool_param(cx, value) else {
                    return Some(Err(DriverError::Silent));
                };
                if h.server.set_option(
                    MYSQLND_OPT_INT_AND_FLOAT_NATIVE,
                    OptionValue::Int(i64::from(!b)),
                ) {
                    Ok(())
                } else {
                    Err(h.error())
                }
            }
            _ => Err(DriverError::Silent),
        };
        Some(r)
    }

    fn last_insert_id(
        &self,
        _cx: &mut DriverCx<'_>,
        _name: Option<&ZStr>,
    ) -> Option<Result<ZStr, DriverError>> {
        Some(Ok(ZStr::from(self.h.server.insert_id().to_string())))
    }

    fn fetch_error(&self, stmt: Option<&dyn StmtDriver>) -> Option<Vec<Value>> {
        let info = match stmt.and_then(|s| s.as_any().downcast_ref::<MysqlStmt>()) {
            Some(s) => s.einfo.borrow().clone(),
            None => self.h.einfo.borrow().clone(),
        };
        if info.errcode == 0 {
            return Some(Vec::new());
        }
        Some(vec![
            Value::Int(i64::from(info.errcode)),
            Value::Str(ZStr::from(info.errmsg.unwrap_or_default())),
        ])
    }

    fn get_attribute(
        &self,
        cx: &mut DriverCx<'_>,
        attr: i64,
    ) -> Option<Result<Option<Value>, DriverError>> {
        let h = &self.h;
        let v = match attr {
            ATTR_CLIENT_VERSION => Value::from(php_mysqlnd::CLIENT_INFO),
            ATTR_SERVER_VERSION => Value::from(h.server.server_info().unwrap_or_default().as_str()),
            ATTR_CONNECTION_STATUS => {
                Value::from(h.server.host_info().unwrap_or_default().as_str())
            }
            ATTR_SERVER_INFO => {
                let r = h.server.statistics();
                flush_diag(cx);
                match r {
                    Ok(s) => Value::Str(ZStr::from(s)),
                    Err(_) => return Some(Err(h.error())),
                }
            }
            ATTR_AUTOCOMMIT => Value::Bool(h.auto_commit.get()),
            ATTR_DEFAULT_STR_PARAM => Value::Int(if h.assume_national.get() {
                PARAM_STR_NATL
            } else {
                PARAM_STR_CHAR
            }),
            ATTR_USE_BUFFERED_QUERY => Value::Bool(h.buffered.get()),
            ATTR_EMULATE_PREPARES => Value::Bool(h.emulate_prepare.get()),
            ATTR_LOCAL_INFILE => Value::Bool(h.local_infile.get()),
            ATTR_LOCAL_INFILE_DIRECTORY => match &h.local_infile_directory {
                Some(d) => Value::Str(ZStr::from(d.as_slice())),
                None => Value::Null,
            },
            ATTR_FETCH_TABLE_NAMES => Value::Bool(h.fetch_table_names.get()),
            _ => return Some(Ok(None)),
        };
        Some(Ok(Some(v)))
    }

    fn check_liveness(&self, cx: &mut DriverCx<'_>) -> bool {
        let r = self.h.server.ping().is_ok();
        flush_diag(cx);
        r
    }

    fn in_transaction(&self, _cx: &mut DriverCx<'_>) -> Option<bool> {
        Some(self.h.server.server_status() & SERVER_STATUS_IN_TRANS != 0)
    }

    fn scanner(&self) -> &dyn Scanner {
        &MysqlScanner
    }
}

// ---------------------------------------------------------------------------
// statements
// ---------------------------------------------------------------------------

/// `pdo_mysql_stmt`.
#[derive(Debug)]
pub struct MysqlStmt {
    h: Rc<Handle>,
    /// The native prepared statement (`S->stmt`; `None` when emulated).
    stmt: Option<Statement>,
    num_params: u32,
    /// The text-protocol result set.
    result: RefCell<Option<ResultSet>>,
    /// `S->fields` (`Some` while `S->result` is set).
    fields: RefCell<Option<Rc<Metadata>>>,
    /// The current row (`S->current_row` / the bound result values).
    current_row: RefCell<Option<Vec<Value>>>,
    einfo: RefCell<ErrorInfo>,
    done: Cell<bool>,
    /// Parameters bound for the current execution (`EXEC_PRE` events).
    bound: Cell<u32>,
}

/// The VM context as a plain [`RuntimeContext`] for mysqlnd parameter
/// conversions.
struct HostCtx<'a>(&'a mut dyn php_sqlite::Host);

impl RuntimeContext for HostCtx<'_> {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.0.diagnostic(level, message)
    }
    fn precision(&self) -> i64 {
        self.0.precision()
    }
    fn serialize_precision(&self) -> i64 {
        self.0.serialize_precision()
    }
    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        self.0.cast_object(obj, target)
    }
}

impl MysqlStmt {
    fn new(h: Rc<Handle>, stmt: Option<Statement>, num_params: u32) -> Self {
        MysqlStmt {
            h,
            stmt,
            num_params,
            result: RefCell::new(None),
            fields: RefCell::new(None),
            current_row: RefCell::new(None),
            einfo: RefCell::new(ErrorInfo::default()),
            done: Cell::new(false),
            bound: Cell::new(0),
        }
    }

    /// Whether this is a native prepared statement.
    pub fn is_native(&self) -> bool {
        self.stmt.is_some()
    }

    /// `pdo_mysql_error_stmt(stmt)`.
    fn error(&self) -> DriverError {
        let (errno, state, native) = match &self.stmt {
            Some(s) => (s.errno(), s.sqlstate(), s.error()),
            None => (
                self.h.server.errno(),
                self.h.server.sqlstate(),
                self.h.server.error(),
            ),
        };
        *self.einfo.borrow_mut() = ErrorInfo {
            errcode: errno,
            errmsg: error_message(errno, self.h.server.more_results(), native),
        };
        DriverError::State(sqlstate_of(&state))
    }

    /// `pdo_mysql_free_result()`.
    fn free_result(&self) {
        let r = self.result.borrow_mut().take();
        if let Some(mut r) = r {
            r.free(false);
        }
        self.fields.borrow_mut().take();
        self.current_row.borrow_mut().take();
    }

    fn column_count(&self) -> usize {
        self.fields.borrow().as_ref().map_or(0, |m| m.fields.len())
    }

    /// `pdo_mysql_fill_stmt_from_result()`.
    fn fill_from_result(&self) -> Result<ExecuteInfo, DriverError> {
        let server = &self.h.server;
        let affected = server.affected_rows();
        if affected != u64::MAX {
            #[allow(clippy::cast_possible_wrap)]
            let n = affected as i64;
            return Ok(ExecuteInfo {
                column_count: 0,
                row_count: Some(n),
            });
        }
        let r = if self.h.buffered.get() {
            server.store_result()
        } else {
            server.use_result()
        };
        let Ok(Some(rs)) = r else {
            return Err(self.error());
        };
        #[allow(clippy::cast_possible_wrap)]
        let row_count = rs.num_rows() as i64;
        let column_count = i64::from(rs.num_fields());
        *self.fields.borrow_mut() = Some(rs.metadata().clone());
        *self.result.borrow_mut() = Some(rs);
        Ok(ExecuteInfo {
            column_count,
            row_count: Some(row_count),
        })
    }

    /// `pdo_mysql_stmt_after_execute_prepared()`.
    fn after_execute_prepared(&self, st: &Statement) -> Result<ExecuteInfo, DriverError> {
        let column_count = i64::from(st.field_count());
        let _ = st.bind_result();
        if st.field_count() > 0 {
            if let Some(meta) = st.metadata() {
                *self.fields.borrow_mut() = Some(meta);
                if self.h.buffered.get() && st.store_result().is_err() {
                    return Err(self.error());
                }
            }
        }
        let affected = st.affected_rows();
        #[allow(clippy::cast_possible_wrap)]
        let row_count = (affected != u64::MAX).then_some(affected as i64);
        Ok(ExecuteInfo {
            column_count,
            row_count,
        })
    }

    fn execute_inner(
        &self,
        cx: &mut DriverCx<'_>,
        active_query: Option<&ZStr>,
    ) -> Result<ExecuteInfo, DriverError> {
        self.free_result();
        self.done.set(false);
        let bound = self.bound.replace(0);
        if let Some(st) = &self.stmt {
            if bound < self.num_params {
                return Err(DriverError::State(SqlState(*b"HY093")));
            }
            let r = {
                let mut ctx = HostCtx(cx.host());
                st.execute(&mut ctx)
            };
            return match r {
                Ok(()) => self.after_execute_prepared(st),
                Err(ExecuteError::Mysql(_)) => Err(self.error()),
                Err(ExecuteError::Runtime(e)) => {
                    let _ = cx.check::<()>(Err(e));
                    Err(self.error())
                }
            };
        }
        let q = active_query.map_or(&[][..], ZStr::as_bytes);
        if self.h.server.query(q).is_err() {
            return Err(self.error());
        }
        self.fill_from_result()
    }

    fn next_rowset_inner(&self) -> Result<ExecuteInfo, DriverError> {
        self.free_result();
        if let Some(st) = &self.stmt {
            st.free_result();
            if !matches!(st.next_result(), Ok(true)) {
                let e = self.error();
                self.done.set(true);
                return Err(e);
            }
            return self.after_execute_prepared(st);
        }
        if !matches!(self.h.server.next_result(), Ok(true)) {
            let e = self.error();
            self.done.set(true);
            return Err(e);
        }
        self.fill_from_result()
    }
}

impl Drop for MysqlStmt {
    fn drop(&mut self) {
        self.free_result();
        if let Some(st) = &self.stmt {
            let _ = st.close();
        }
        if !self.done.get() && self.h.server.is_connected() {
            self.h.drain_results();
        }
    }
}

/// `type_to_name_native()`.
fn native_type_name(t: u8) -> Option<&'static str> {
    Some(match t {
        MYSQL_TYPE_STRING => "STRING",
        MYSQL_TYPE_VAR_STRING => "VAR_STRING",
        MYSQL_TYPE_TINY => "TINY",
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_SHORT => "SHORT",
        MYSQL_TYPE_LONG => "LONG",
        MYSQL_TYPE_LONGLONG => "LONGLONG",
        MYSQL_TYPE_INT24 => "INT24",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_DECIMAL => "DECIMAL",
        MYSQL_TYPE_NEWDECIMAL => "NEWDECIMAL",
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        MYSQL_TYPE_TIMESTAMP => "TIMESTAMP",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_SET => "SET",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_DATE => "DATE",
        MYSQL_TYPE_NEWDATE => "NEWDATE",
        MYSQL_TYPE_VECTOR => "VECTOR",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_TIME => "TIME",
        MYSQL_TYPE_DATETIME => "DATETIME",
        MYSQL_TYPE_TINY_BLOB => "TINY_BLOB",
        MYSQL_TYPE_MEDIUM_BLOB => "MEDIUM_BLOB",
        MYSQL_TYPE_LONG_BLOB => "LONG_BLOB",
        MYSQL_TYPE_BLOB => "BLOB",
        MYSQL_TYPE_NULL => "NULL",
        _ => return None,
    })
}

/// The `pdo_type` `getColumnMeta()` reports for a native type.
fn pdo_type_of(t: u8) -> i64 {
    match t {
        MYSQL_TYPE_BIT | MYSQL_TYPE_YEAR | MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT
        | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG => PARAM_INT,
        _ => PARAM_STR,
    }
}

fn colno_index(colno: i64, count: usize) -> Option<usize> {
    usize::try_from(colno).ok().filter(|&c| c < count)
}

impl StmtDriver for MysqlStmt {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn execute(
        &self,
        cx: &mut DriverCx<'_>,
        _executed: bool,
        active_query: Option<&ZStr>,
    ) -> Result<ExecuteInfo, DriverError> {
        let r = self.execute_inner(cx, active_query);
        flush_diag(cx);
        r
    }

    fn fetch(&self, cx: &mut DriverCx<'_>, _ori: i64, _offset: i64) -> Result<bool, DriverError> {
        if self.fields.borrow().is_none() {
            return Ok(false);
        }
        let r = if let Some(st) = &self.stmt {
            match st.fetch() {
                Ok(Some(row)) => {
                    *self.current_row.borrow_mut() = Some(row);
                    Ok(true)
                }
                Ok(None) | Err(_) => Err(self.error()),
            }
        } else {
            let row = match self.result.borrow_mut().as_mut() {
                Some(rs) => rs.fetch_row_values(),
                None => Ok(None),
            };
            match row {
                Ok(Some(row)) => {
                    *self.current_row.borrow_mut() = Some(row);
                    Ok(true)
                }
                Ok(None) => Ok(false),
                Err(_) => Err(self.error()),
            }
        };
        flush_diag(cx);
        r
    }

    fn describe(&self, _cx: &mut DriverCx<'_>, colno: i64) -> Result<ColumnData, DriverError> {
        let fields = self.fields.borrow();
        let Some(meta) = fields.as_ref() else {
            return Err(DriverError::Silent);
        };
        let Some(i) = colno_index(colno, meta.fields.len()) else {
            return Err(DriverError::Silent);
        };
        let f = &meta.fields[i];
        let name = if self.h.fetch_table_names.get() {
            let mut n = f.table.clone();
            n.push(b'.');
            n.extend_from_slice(&f.name);
            n
        } else {
            f.name.clone()
        };
        Ok(ColumnData {
            name: ZStr::from(name),
            maxlen: i64::from(f.length),
            precision: i64::from(f.decimals),
        })
    }

    fn get_col(&self, _cx: &mut DriverCx<'_>, colno: i64) -> Result<Value, DriverError> {
        if self.fields.borrow().is_none() {
            return Err(DriverError::Silent);
        }
        let Some(i) = colno_index(colno, self.column_count()) else {
            return Err(DriverError::Silent);
        };
        let row = self.current_row.borrow();
        Ok(row
            .as_ref()
            .and_then(|r| r.get(i).cloned())
            .unwrap_or(Value::Null))
    }

    fn param_hook(
        &self,
        cx: &mut DriverCx<'_>,
        param: &mut BoundParam,
        event: ParamEvent,
        _executed: bool,
    ) -> Option<Result<(), DriverError>> {
        let Some(st) = &self.stmt else {
            return Some(Ok(()));
        };
        if !param.is_param {
            return Some(Ok(()));
        }
        let paramno = u32::try_from(param.paramno)
            .ok()
            .filter(|&n| n < self.num_params);
        match event {
            ParamEvent::Alloc => {
                if paramno.is_none() {
                    return Some(Err(DriverError::State(SqlState(*b"HY093"))));
                }
                Some(Ok(()))
            }
            ParamEvent::ExecPre => {
                self.bound.set(self.bound.get() + 1);
                let Some(no) = paramno else {
                    return Some(Err(DriverError::State(SqlState(*b"HY093"))));
                };
                let ty = param_type(param.param_type);
                let mut v = param.parameter.deref();
                if ty == PARAM_NULL || v.is_null() {
                    let _ = st.bind_one_param(no, Value::Null, MYSQL_TYPE_NULL);
                    return Some(Ok(()));
                }
                match ty {
                    PARAM_STMT => return Some(Err(DriverError::Silent)),
                    PARAM_LOB => {
                        if matches!(v, Value::Resource(_)) {
                            let Some(mem) = cx.host().read_stream(&v) else {
                                return Some(Err(DriverError::Impl(
                                    "HY105",
                                    "Expected a stream resource".to_string(),
                                )));
                            };
                            v = Value::Str(ZStr::from(mem));
                            match &param.parameter {
                                Value::Reference(r) => {
                                    let _ = r.set(v.clone());
                                }
                                _ => param.parameter = v.clone(),
                            }
                        }
                    }
                    _ => {}
                }
                let wire = match v {
                    Value::Str(_) => MYSQL_TYPE_VAR_STRING,
                    Value::Int(_) => MYSQL_TYPE_LONGLONG,
                    Value::Bool(_) => MYSQL_TYPE_TINY,
                    Value::Float(_) => MYSQL_TYPE_DOUBLE,
                    _ => return Some(Err(DriverError::Silent)),
                };
                let _ = st.bind_one_param(no, v, wire);
                Some(Ok(()))
            }
            _ => Some(Ok(())),
        }
    }

    fn column_meta(
        &self,
        _cx: &mut DriverCx<'_>,
        colno: i64,
        _executed: bool,
    ) -> Option<Result<ZArray, DriverError>> {
        let fields = self.fields.borrow();
        let Some(meta) = fields.as_ref() else {
            return Some(Err(DriverError::Silent));
        };
        let Some(i) = colno_index(colno, meta.fields.len()) else {
            return Some(Err(DriverError::Silent));
        };
        let f = &meta.fields[i];
        let mut t = HashTable::new();
        let mut flags = HashTable::new();
        if !f.def.is_empty() {
            t.insert(
                ArrayKey::from_bytes(b"mysql:def"),
                Value::Str(ZStr::from(f.def.as_slice())),
            );
        }
        for (bit, name) in [
            (NOT_NULL_FLAG, "not_null"),
            (PRI_KEY_FLAG, "primary_key"),
            (MULTIPLE_KEY_FLAG, "multiple_key"),
            (UNIQUE_KEY_FLAG, "unique_key"),
            (BLOB_FLAG, "blob"),
        ] {
            if f.flags & bit != 0 {
                let _ = flags.push(Value::from(name));
            }
        }
        if let Some(n) = native_type_name(f.type_) {
            t.insert(ArrayKey::from_bytes(b"native_type"), Value::from(n));
        }
        t.insert(
            ArrayKey::from_bytes(b"pdo_type"),
            Value::Int(pdo_type_of(f.type_)),
        );
        t.insert(
            ArrayKey::from_bytes(b"flags"),
            Value::Array(ZArray::from_table(flags)),
        );
        t.insert(
            ArrayKey::from_bytes(b"table"),
            Value::Str(ZStr::from(f.table.as_slice())),
        );
        Some(Ok(ZArray::from_table(t)))
    }

    fn next_rowset(&self, cx: &mut DriverCx<'_>) -> Option<Result<ExecuteInfo, DriverError>> {
        let r = self.next_rowset_inner();
        flush_diag(cx);
        Some(r)
    }

    fn cursor_closer(&self, cx: &mut DriverCx<'_>) -> Option<Result<(), DriverError>> {
        self.done.set(true);
        self.free_result();
        if let Some(st) = &self.stmt {
            st.free_result();
        }
        let r = if self.h.drain_results() {
            Ok(())
        } else {
            Err(self.error())
        };
        flush_diag(cx);
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_sqlite::pdo::parser::tokenize;

    #[test]
    fn dsn_defaults() {
        let d = parse_dsn(b"dbname=test", Some(b"/tmp/s.sock"));
        assert_eq!(d.host, b"localhost");
        assert_eq!(d.port, 3306);
        assert_eq!(d.dbname, b"test");
        assert_eq!(d.unix_socket.as_deref(), Some(&b"/tmp/s.sock"[..]));
        assert_eq!(d.charset, None);
        assert_eq!(d.user, None);
    }

    #[test]
    fn dsn_tcp_host_ignores_socket() {
        let d = parse_dsn(
            b"host=127.0.0.1;port=3307x;dbname=a;;b;charset=utf8mb4;user=u;password=p",
            Some(b"/tmp/s.sock"),
        );
        assert_eq!(d.host, b"127.0.0.1");
        assert_eq!(d.port, 3307);
        assert_eq!(d.dbname, b"a;b");
        assert_eq!(d.unix_socket, None);
        assert_eq!(d.charset.as_deref(), Some(&b"utf8mb4"[..]));
        assert_eq!(d.user.as_deref(), Some(&b"u"[..]));
        assert_eq!(d.password.as_deref(), Some(&b"p"[..]));
    }

    #[test]
    fn dsn_explicit_socket() {
        let d = parse_dsn(b"unix_socket=/x.sock", None);
        assert_eq!(d.unix_socket.as_deref(), Some(&b"/x.sock"[..]));
        assert_eq!(atoi(b"  -12ab"), -12);
    }

    #[test]
    fn quoting_styles() {
        assert_eq!(wrap_quoted(b"a\\'b", PARAM_STR, false), b"'a\\'b'");
        assert_eq!(
            wrap_quoted(b"x", PARAM_STR | PARAM_STR_NATL, false),
            b"N'x'"
        );
        assert_eq!(wrap_quoted(b"x", PARAM_STR, true), b"N'x'");
        assert_eq!(wrap_quoted(b"x", PARAM_STR | PARAM_STR_CHAR, true), b"'x'");
        assert_eq!(wrap_quoted(b"x", PARAM_LOB, true), b"_binary'x'");
    }

    #[test]
    fn escaping_matches_mysqlnd() {
        let cs = php_mysqlnd::charset::find_by_name(b"utf8mb4").expect("utf8mb4");
        let e = php_mysqlnd::charset::escape_slashes(cs, b"it's \"q\"\n\0\\");
        assert_eq!(e, b"it\\'s \\\"q\\\"\\n\\0\\\\");
        let e = php_mysqlnd::charset::escape_quotes(cs, b"it's");
        assert_eq!(e, b"it''s");
    }

    fn toks(q: &[u8]) -> Vec<(Token, Vec<u8>)> {
        tokenize(&MysqlScanner, q)
    }

    #[test]
    fn scanner_placeholders() {
        let t = toks(b"SELECT ?, :name FROM t WHERE a = 'x?' AND b = \"y:z\"");
        let kinds: Vec<_> = t.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds.iter().filter(|k| **k == Token::BindPos).count(), 1);
        assert_eq!(kinds.iter().filter(|k| **k == Token::Bind).count(), 1);
        assert!(t.contains(&(Token::Text, b"'x?'".to_vec())));
        assert!(t.contains(&(Token::Text, b"\"y:z\"".to_vec())));
    }

    #[test]
    fn scanner_backslash_and_backticks() {
        let t = toks(b"'a\\'?' `c``?` ?");
        assert_eq!(t[0], (Token::Text, b"'a\\'?'".to_vec()));
        assert!(t.contains(&(Token::Text, b"`c``?`".to_vec())));
        assert_eq!(t.last().map(|x| x.0), Some(Token::BindPos));
    }

    #[test]
    fn scanner_comments() {
        let t = toks(b"SELECT 1 # ? comment\n, ?");
        assert!(t.contains(&(Token::Text, b"# ? comment".to_vec())));
        assert_eq!(t.iter().filter(|x| x.0 == Token::BindPos).count(), 1);
        let t = toks(b"SELECT 1 -- ?\n, ?");
        assert!(t.contains(&(Token::Text, b"-- ?".to_vec())));
        // "--" without a following space is not a comment.
        let t = toks(b"SELECT 1--?");
        assert_eq!(t.iter().filter(|x| x.0 == Token::BindPos).count(), 1);
        let t = toks(b"/* ? */ ?");
        assert_eq!(t[0], (Token::Text, b"/* ? */".to_vec()));
        // An unterminated comment ends the scan.
        let t = toks(b"? # ?");
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn scanner_multichar() {
        let t = toks(b"a ?? b ::c");
        assert!(t.contains(&(Token::Text, b"??".to_vec())));
        assert!(t.contains(&(Token::Text, b"::".to_vec())));
    }

    #[test]
    fn native_types() {
        assert_eq!(native_type_name(MYSQL_TYPE_LONG), Some("LONG"));
        assert_eq!(native_type_name(MYSQL_TYPE_VAR_STRING), Some("VAR_STRING"));
        assert_eq!(native_type_name(15), None); // VARCHAR is not named
        assert_eq!(pdo_type_of(MYSQL_TYPE_LONGLONG), PARAM_INT);
        assert_eq!(pdo_type_of(MYSQL_TYPE_NEWDECIMAL), PARAM_STR);
        assert_eq!(sqlstate_of("42S02").as_str(), "42S02");
        assert_eq!(sqlstate_of("").as_str(), "00000");
    }

    #[test]
    fn error_messages() {
        assert_eq!(error_message(0, false, b"x".to_vec()), None);
        assert_eq!(
            error_message(2014, true, Vec::new()).as_deref(),
            Some(MSG_PENDING_RESULT_SETS.as_bytes())
        );
        assert_eq!(
            error_message(2014, false, Vec::new()).as_deref(),
            Some(MSG_UNBUFFERED_ACTIVE.as_bytes())
        );
        assert_eq!(
            error_message(1146, false, b"no".to_vec()).as_deref(),
            Some(&b"no"[..])
        );
    }
}

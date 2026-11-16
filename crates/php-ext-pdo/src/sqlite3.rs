// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/sqlite3`: the `SQLite3`, `SQLite3Stmt`, `SQLite3Result` and
//! `SQLite3Exception` classes and the `SQLITE3_*` constants.

use crate::host::{bind_callable, with_sqlite};
use php_runtime::{Obj, Value};
use php_sqlite::constants::{
    SQLITE3STMT_CLASS_CONSTANTS, SQLITE3_BOTH, SQLITE3_CLASS_CONSTANTS, SQLITE3_OPEN_CREATE,
    SQLITE3_OPEN_READONLY, SQLITE3_OPEN_READWRITE,
};
use php_sqlite::{ParamKey, Sqlite3, Sqlite3Result, Sqlite3Stmt};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, VmResult};
use php_vm::Vm;
use std::rc::Rc;

const NOT_INIT_DB: &str =
    "The SQLite3 object has not been correctly initialised or is already closed";
const NOT_INIT_RESULT: &str =
    "The SQLite3Result object has not been correctly initialised or is already closed";

/// The internal state of a `SQLite3Stmt` object (`None` until prepared).
#[derive(Debug, Default)]
struct StmtState(Option<Sqlite3Stmt>);

/// The internal state of a `SQLite3Result` object.
#[derive(Debug, Default)]
struct ResultState(Option<Rc<Sqlite3Result>>);

fn db_create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    php_vm::object::set_internal(o, Sqlite3::new());
    Ok(())
}

fn stmt_create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    php_vm::object::set_internal(o, StmtState::default());
    Ok(())
}

fn result_create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    php_vm::object::set_internal(o, ResultState::default());
    Ok(())
}

fn db_of(o: &Obj) -> Sqlite3 {
    php_vm::object::internal::<Sqlite3, _>(o, |d| d.clone()).unwrap_or_else(|| {
        let d = Sqlite3::new();
        php_vm::object::set_internal(o, d.clone());
        d
    })
}

fn this_db(a: &Args) -> Sqlite3 {
    db_of(&a.this_obj())
}

fn this_stmt(vm: &mut Vm, a: &Args) -> VmResult<Sqlite3Stmt> {
    let o = a.this_obj();
    match php_vm::object::internal::<StmtState, _>(&o, |s| s.0.clone()).flatten() {
        Some(s) => Ok(s),
        None => Err(vm.throw("Error", NOT_INIT_DB)),
    }
}

fn this_result(vm: &mut Vm, a: &Args) -> VmResult<Rc<Sqlite3Result>> {
    let o = a.this_obj();
    match php_vm::object::internal::<ResultState, _>(&o, |s| s.0.clone()).flatten() {
        Some(s) => Ok(s),
        None => Err(vm.throw("Error", NOT_INIT_RESULT)),
    }
}

/// The NUL-byte check of a `p` (path) ZPP argument.
fn check_path(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Vec<u8>> {
    let s = a.str(n).into_bytes();
    if s.contains(&0) {
        return Err(crate::host::arg_value_error(
            vm,
            a,
            n + 1,
            "must not contain any null bytes",
        ));
    }
    Ok(s)
}

fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

fn new_stmt_object(vm: &mut Vm, s: Sqlite3Stmt) -> VmResult<Value> {
    let Some(cls) = vm.find_class(b"SQLite3Stmt") else {
        return Ok(Value::Bool(false));
    };
    let o = vm.new_object(&cls)?;
    php_vm::object::set_internal(&o, StmtState(Some(s)));
    Ok(Value::Object(o))
}

fn new_result_object(vm: &mut Vm, r: Sqlite3Result) -> VmResult<Value> {
    let Some(cls) = vm.find_class(b"SQLite3Result") else {
        return Ok(Value::Bool(false));
    };
    let o = vm.new_object(&cls)?;
    php_vm::object::set_internal(&o, ResultState(Some(Rc::new(r))));
    Ok(Value::Object(o))
}

// ---------------------------------------------------------------------------
// SQLite3
// ---------------------------------------------------------------------------

fn open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let filename = check_path(vm, a, 0)?;
    let flags = int_or(a, 1, SQLITE3_OPEN_READWRITE | SQLITE3_OPEN_CREATE);
    let key = a.str(2);
    with_sqlite(vm, |h| db.open(h, &filename, flags, key.as_bytes()))?;
    Ok(Value::Null)
}

fn close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    with_sqlite(vm, |h| db.close(h)).map(Value::Bool)
}

fn exec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let sql = a.str(0);
    with_sqlite(vm, |h| db.exec(h, sql.as_bytes())).map(Value::Bool)
}

fn version(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(Sqlite3::version()))
}

fn last_insert_row_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    with_sqlite(vm, |_| db.last_insert_row_id()).map(Value::Int)
}

fn last_error_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    with_sqlite(vm, |_| db.last_error_code()).map(Value::Int)
}

fn last_extended_error_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    with_sqlite(vm, |_| db.last_extended_error_code()).map(Value::Int)
}

fn enable_extended_result_codes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let enable = if a.has(0) { a.bool(0) } else { true };
    with_sqlite(vm, |_| db.enable_extended_result_codes(enable)).map(Value::Bool)
}

fn last_error_msg(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    with_sqlite(vm, |_| db.last_error_msg()).map(Value::Str)
}

fn changes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    with_sqlite(vm, |_| db.changes()).map(Value::Int)
}

fn busy_timeout(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let ms = a.int(0);
    with_sqlite(vm, |_| db.busy_timeout(ms)).map(Value::Bool)
}

fn load_extension(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let name = a.str(0);
    with_sqlite(vm, |h| db.load_extension(h, name.as_bytes())).map(Value::Bool)
}

fn backup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let Value::Object(dest) = a.at(0) else {
        return Ok(Value::Bool(false));
    };
    let dest = db_of(&dest);
    let src_name = if a.has(1) {
        check_path(vm, a, 1)?
    } else {
        b"main".to_vec()
    };
    let dst_name = if a.has(2) {
        check_path(vm, a, 2)?
    } else {
        b"main".to_vec()
    };
    with_sqlite(vm, |h| db.backup(h, &dest, &src_name, &dst_name)).map(Value::Bool)
}

fn escape_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    with_sqlite(vm, |_| Sqlite3::escape_string(s.as_bytes())).map(Value::Str)
}

fn prepare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let sql = a.str(0);
    match with_sqlite(vm, |h| db.prepare(h, sql.as_bytes()))? {
        Some(s) => new_stmt_object(vm, s),
        None => Ok(Value::Bool(false)),
    }
}

fn query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let sql = a.str(0);
    let used = crate::host::used_ret(vm);
    match with_sqlite(vm, |h| db.query(h, sql.as_bytes(), used))? {
        Some(r) => new_result_object(vm, r),
        None => Ok(Value::Bool(false)),
    }
}

fn query_single(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let sql = a.str(0);
    let entire = a.bool(1);
    let used = crate::host::used_ret(vm);
    with_sqlite(vm, |h| db.query_single(h, sql.as_bytes(), entire, used))
}

fn create_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let name = a.str(0);
    let cb = bind_callable(vm, a.at(1));
    let argc = int_or(a, 2, -1);
    let flags = a.int(3);
    with_sqlite(vm, |_| db.create_function(name.as_bytes(), cb, argc, flags)).map(Value::Bool)
}

fn create_aggregate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let name = a.str(0);
    let step = bind_callable(vm, a.at(1));
    let fini = bind_callable(vm, a.at(2));
    let argc = int_or(a, 3, -1);
    with_sqlite(vm, |_| {
        db.create_aggregate(name.as_bytes(), step, fini, argc)
    })
    .map(Value::Bool)
}

fn create_collation(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let name = a.str(0);
    let cb = bind_callable(vm, a.at(1));
    with_sqlite(vm, |_| db.create_collation(name.as_bytes(), cb)).map(Value::Bool)
}

fn open_blob(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let table = a.str(0);
    let column = a.str(1);
    let rowid = a.int(2);
    let dbname = if a.has(3) {
        check_path(vm, a, 3)?
    } else {
        b"main".to_vec()
    };
    let flags = int_or(a, 4, SQLITE3_OPEN_READONLY);
    let blob = with_sqlite(vm, |h| {
        db.open_blob(
            h,
            table.as_bytes(),
            column.as_bytes(),
            rowid,
            &dbname,
            flags,
        )
    })?;
    Ok(match blob {
        Some(b) => crate::streams::new_stream(b.into_stream()),
        None => Value::Bool(false),
    })
}

fn enable_exceptions(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let enable = a.bool(0);
    with_sqlite(vm, |h| db.enable_exceptions(h, enable)).map(Value::Bool)
}

fn set_authorizer(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let db = this_db(a);
    let cb = a.opt(0).map(|cb| bind_callable(vm, cb));
    with_sqlite(vm, |_| db.set_authorizer(cb)).map(Value::Bool)
}

// ---------------------------------------------------------------------------
// SQLite3Stmt
// ---------------------------------------------------------------------------

fn stmt_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(dbo) = a.at(0) else {
        return Ok(Value::Null);
    };
    let db = db_of(&dbo);
    let sql = a.str(1);
    let r = with_sqlite(vm, |h| Sqlite3Stmt::construct(h, &db, sql.as_bytes()))?;
    if let Some(s) = r {
        let o = a.this_obj();
        php_vm::object::set_internal(&o, StmtState(Some(s)));
    }
    Ok(Value::Null)
}

fn param_key(v: &Value) -> (Option<i64>, Vec<u8>) {
    match v.deref() {
        Value::Int(i) => (Some(i), Vec::new()),
        Value::Str(s) => (None, s.into_bytes()),
        _ => (None, Vec::new()),
    }
}

fn bind(vm: &mut Vm, a: &Args, by_ref: bool) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    let (num, name) = param_key(&a.at(0));
    let key = match num {
        Some(n) => ParamKey::Number(n),
        None => ParamKey::Name(&name),
    };
    let ty = if a.has(2) { Some(a.int(2)) } else { None };
    let r = if by_ref {
        let var = a.values.get(1).cloned().unwrap_or(Value::Null);
        with_sqlite(vm, |_| s.bind_param(key, var, ty))
    } else {
        let value = a.at(1);
        with_sqlite(vm, |_| s.bind_value(key, &value, ty))
    };
    r.map(Value::Bool)
}

fn bind_param(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    bind(vm, a, true)
}

fn bind_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    bind(vm, a, false)
}

fn stmt_clear(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |h| s.clear(h)).map(Value::Bool)
}

fn stmt_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |_| s.close()).map(Value::Bool)
}

fn stmt_execute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    match with_sqlite(vm, |h| s.execute(h))? {
        Some(r) => new_result_object(vm, r),
        None => Ok(Value::Bool(false)),
    }
}

fn stmt_get_sql(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    let expand = a.bool(0);
    Ok(match with_sqlite(vm, |h| s.get_sql(h, expand))? {
        Some(sql) => Value::Str(sql),
        None => Value::Bool(false),
    })
}

fn stmt_param_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |_| s.param_count()).map(Value::Int)
}

fn stmt_read_only(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |_| s.read_only()).map(Value::Bool)
}

fn stmt_reset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |h| s.reset(h)).map(Value::Bool)
}

fn stmt_busy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |_| s.busy()).map(Value::Bool)
}

fn stmt_explain(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |_| s.explain()).map(Value::Int)
}

fn stmt_set_explain(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mode = a.int(0);
    if !(0..=2).contains(&mode) {
        return Err(crate::host::arg_value_error(
            vm,
            a,
            1,
            "must be one of the SQLite3Stmt::EXPLAIN_MODE_* constants",
        ));
    }
    let s = this_stmt(vm, a)?;
    with_sqlite(vm, |_| s.set_explain(mode)).map(Value::Bool)
}

// ---------------------------------------------------------------------------
// SQLite3Result
// ---------------------------------------------------------------------------

fn result_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Exception", "SQLite3Result cannot be directly instantiated"))
}

fn result_num_columns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    with_sqlite(vm, |_| r.num_columns()).map(Value::Int)
}

fn result_column_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    let col = a.int(0);
    Ok(match with_sqlite(vm, |_| r.column_name(col))? {
        Some(n) => Value::Str(n),
        None => Value::Bool(false),
    })
}

fn result_column_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    let col = a.int(0);
    Ok(match with_sqlite(vm, |_| r.column_type(col))? {
        Some(t) => Value::Int(t),
        None => Value::Bool(false),
    })
}

fn result_fetch_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    let mode = int_or(a, 0, SQLITE3_BOTH);
    with_sqlite(vm, |h| r.fetch_array(h, mode))
}

fn result_fetch_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    let mode = int_or(a, 0, SQLITE3_BOTH);
    with_sqlite(vm, |h| r.fetch_all(h, mode))
}

fn result_reset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    with_sqlite(vm, |h| r.reset(h)).map(Value::Bool)
}

fn result_finalize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_result(vm, a)?;
    with_sqlite(vm, |h| r.finalize(h)).map(Value::Bool)
}

/// Registers the `ext/sqlite3` classes and constants.
#[allow(clippy::too_many_lines)] // one declarative class table
pub fn register(vm: &mut Vm) {
    let pub_ = acc::PUBLIC;
    let no_ser = class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE;

    ClassBuilder::new("SQLite3Exception")
        .extends(vm, "Exception")
        .register(vm);

    let mut db = ClassBuilder::new("SQLite3")
        .flags(no_ser)
        .hooks(ClassHooks {
            create: Some(db_create),
            ..Default::default()
        });
    for &(name, v) in SQLITE3_CLASS_CONSTANTS {
        db = db.constant(name, Value::Int(v));
    }
    db.method(
        "__construct(string $filename, int $flags = SQLITE3_OPEN_READWRITE | SQLITE3_OPEN_CREATE, #[\\SensitiveParameter] string $encryptionKey = \"\")",
        pub_,
        open,
    )
    .method(
        "open(string $filename, int $flags = SQLITE3_OPEN_READWRITE | SQLITE3_OPEN_CREATE, #[\\SensitiveParameter] string $encryptionKey = \"\"): void",
        pub_,
        open,
    )
    .method("close(): bool", pub_, close)
    .method("version(): array", pub_ | acc::STATIC, version)
    .method("lastInsertRowID(): int", pub_, last_insert_row_id)
    .method("lastErrorCode(): int", pub_, last_error_code)
    .method("lastExtendedErrorCode(): int", pub_, last_extended_error_code)
    .method("lastErrorMsg(): string", pub_, last_error_msg)
    .method("changes(): int", pub_, changes)
    .method("busyTimeout(int $milliseconds): bool", pub_, busy_timeout)
    .method("loadExtension(string $name): bool", pub_, load_extension)
    .method(
        "backup(SQLite3 $destination, string $sourceDatabase = \"main\", string $destinationDatabase = \"main\"): bool",
        pub_,
        backup,
    )
    .method(
        "escapeString(string $string): string",
        pub_ | acc::STATIC,
        escape_string,
    )
    .method("prepare(string $query): SQLite3Stmt|false", pub_, prepare)
    .method("exec(string $query): bool", pub_, exec)
    .method("query(string $query): SQLite3Result|false", pub_, query)
    .method(
        "querySingle(string $query, bool $entireRow = false): mixed",
        pub_,
        query_single,
    )
    .method(
        "createFunction(string $name, callable $callback, int $argCount = -1, int $flags = 0): bool",
        pub_,
        create_function,
    )
    .method(
        "createAggregate(string $name, callable $stepCallback, callable $finalCallback, int $argCount = -1): bool",
        pub_,
        create_aggregate,
    )
    .method(
        "createCollation(string $name, callable $callback): bool",
        pub_,
        create_collation,
    )
    .method(
        "openBlob(string $table, string $column, int $rowid, string $database = \"main\", int $flags = SQLITE3_OPEN_READONLY)",
        pub_,
        open_blob,
    )
    .method(
        "enableExceptions(bool $enable = false): bool",
        pub_,
        enable_exceptions,
    )
    .method(
        "enableExtendedResultCodes(bool $enable = true): bool",
        pub_,
        enable_extended_result_codes,
    )
    .method("setAuthorizer(?callable $callback): bool", pub_, set_authorizer)
    .register(vm);

    let mut st = ClassBuilder::new("SQLite3Stmt")
        .flags(no_ser)
        .hooks(ClassHooks {
            create: Some(stmt_create),
            ..Default::default()
        });
    for &(name, v) in SQLITE3STMT_CLASS_CONSTANTS {
        st = st.constant(name, Value::Int(v));
    }
    st.method(
        "__construct(SQLite3 $sqlite3, string $query)",
        acc::PRIVATE,
        stmt_construct,
    )
    .method(
        "bindParam(string|int $param, mixed &$var, int $type = SQLITE3_TEXT): bool",
        pub_,
        bind_param,
    )
    .method(
        "bindValue(string|int $param, mixed $value, int $type = SQLITE3_TEXT): bool",
        pub_,
        bind_value,
    )
    .method("clear(): bool", pub_, stmt_clear)
    .method("close(): true", pub_, stmt_close)
    .method("execute(): SQLite3Result|false", pub_, stmt_execute)
    .method(
        "getSQL(bool $expand = false): string|false",
        pub_,
        stmt_get_sql,
    )
    .method("paramCount(): int", pub_, stmt_param_count)
    .method("readOnly(): bool", pub_, stmt_read_only)
    .method("reset(): bool", pub_, stmt_reset)
    .method("busy(): bool", pub_, stmt_busy)
    .method("explain(): int", pub_, stmt_explain)
    .method("setExplain(int $mode): bool", pub_, stmt_set_explain)
    .register(vm);

    ClassBuilder::new("SQLite3Result")
        .flags(no_ser)
        .hooks(ClassHooks {
            create: Some(result_create),
            ..Default::default()
        })
        .method("__construct()", acc::PRIVATE, result_construct)
        .method("numColumns(): int", pub_, result_num_columns)
        .method(
            "columnName(int $column): string|false",
            pub_,
            result_column_name,
        )
        .method(
            "columnType(int $column): int|false",
            pub_,
            result_column_type,
        )
        .method(
            "fetchArray(int $mode = SQLITE3_BOTH): array|false",
            pub_,
            result_fetch_array,
        )
        .method(
            "fetchAll(int $mode = SQLITE3_BOTH): array|false",
            pub_,
            result_fetch_all,
        )
        .method("reset(): bool", pub_, result_reset)
        .method("finalize(): true", pub_, result_finalize)
        .register(vm);
}

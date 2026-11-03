//! The `PDO`, `PDOException` and `Pdo\Sqlite` classes and `pdo_drivers()`.

use crate::host::{bind_callable, pdo_of, with_host, with_sqlite};
use php_pdo::consts::{pdo_class_constants, ConstValue, PARAM_STR};
use php_pdo::{ConnectArgs, Pdo};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_sqlite::pdo::PdoSqliteDbh;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, VmResult};
use php_vm::Vm;

/// The `create_object` handler of `PDO` (`pdo_dbh_new`).
fn pdo_create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let p = Pdo::new();
    p.attach_object(o);
    php_vm::object::set_internal(o, p);
    Ok(())
}

fn this_pdo(a: &Args) -> (Obj, Pdo) {
    let o = a.this_obj();
    let p = pdo_of(&o).unwrap_or_else(|| {
        let p = Pdo::new();
        p.attach_object(&o);
        php_vm::object::set_internal(&o, p.clone());
        p
    });
    (o, p)
}

/// The optional string argument `n` (`?string`).
fn opt_str(a: &Args, n: usize) -> Option<ZStr> {
    a.opt(n).map(|_| a.str(n))
}

fn connect_args<'a>(
    dsn: &'a ZStr,
    user: Option<&'a ZStr>,
    pass: Option<&'a ZStr>,
    options: Option<&'a ZArray>,
) -> ConnectArgs<'a> {
    ConnectArgs {
        dsn: dsn.as_bytes(),
        username: user.map(ZStr::as_bytes),
        password: pass.map(ZStr::as_bytes),
        options,
    }
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, p) = this_pdo(a);
    let dsn = a.str(0);
    let user = opt_str(a, 1);
    let pass = opt_str(a, 2);
    let options = a.opt(3).map(|_| a.array(3));
    let cls = o.class_name();
    with_host(vm, |h| {
        p.construct(
            h,
            &cls,
            connect_args(&dsn, user.as_ref(), pass.as_ref(), options.as_ref()),
        )
    })?;
    Ok(Value::Null)
}

fn connect(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dsn = a.str(0);
    let user = opt_str(a, 1);
    let pass = opt_str(a, 2);
    let options = a.opt(3).map(|_| a.array(3));
    let cls = a
        .called
        .as_ref()
        .map_or_else(|| ZStr::from("PDO"), |c| c.name.clone());
    with_host(vm, |h| {
        Pdo::connect(
            h,
            &cls,
            connect_args(&dsn, user.as_ref(), pass.as_ref(), options.as_ref()),
        )
    })
}

fn begin_transaction(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    with_host(vm, |h| p.begin_transaction(h)).map(Value::Bool)
}

fn commit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    with_host(vm, |h| p.commit(h)).map(Value::Bool)
}

fn roll_back(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    with_host(vm, |h| p.roll_back(h)).map(Value::Bool)
}

fn in_transaction(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    with_host(vm, |h| p.in_transaction(h)).map(Value::Bool)
}

fn error_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    with_host(vm, |h| p.error_code(h))
}

fn error_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    with_host(vm, |h| p.error_info(h)).map(Value::Array)
}

fn exec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let sql = a.str(0);
    with_host(vm, |h| p.exec(h, &sql))
}

fn get_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let attr = a.int(0);
    with_host(vm, |h| p.get_attribute(h, attr))
}

fn set_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let attr = a.int(0);
    let value = a.at(1);
    with_host(vm, |h| p.set_attribute(h, attr, &value)).map(Value::Bool)
}

fn get_available_drivers(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(Pdo::get_available_drivers()))
}

fn last_insert_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let name = opt_str(a, 0);
    with_host(vm, |h| p.last_insert_id(h, name.as_ref()))
}

fn prepare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let sql = a.str(0);
    let options = a.opt(1).map(|_| a.array(1));
    with_host(vm, |h| p.prepare(h, &sql, options.as_ref()))
}

fn query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let sql = a.str(0);
    let mode = a.opt(1).map(|_| a.int(1));
    let rest: Vec<Value> = a.values.iter().skip(2).map(Value::deref).collect();
    with_host(vm, |h| p.query(h, &sql, mode, &rest))
}

fn quote(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, p) = this_pdo(a);
    let s = a.str(0);
    let ty = if a.has(1) { a.int(1) } else { PARAM_STR };
    with_host(vm, |h| p.quote(h, &s, ty))
}

fn pdo_drivers(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(Pdo::get_available_drivers()))
}

// ---------------------------------------------------------------------------
// Pdo\Sqlite
// ---------------------------------------------------------------------------

/// `PDO_CONSTRUCT_CHECK` + the `SQLite` driver handle.
fn sqlite_dbh(vm: &mut Vm, a: &Args) -> VmResult<PdoSqliteDbh> {
    let (o, p) = this_pdo(a);
    if let Some(h) = p.sqlite() {
        return Ok(h);
    }
    let cls = o.class_name();
    Err(vm.throw(
        "Error",
        &format!("{} object is uninitialized", cls.to_string_lossy()),
    ))
}

/// The NUL-byte check of a `p` (path) ZPP argument.
fn check_path(vm: &mut Vm, a: &Args, n: usize, s: &ZStr) -> VmResult<()> {
    if s.as_bytes().contains(&0) {
        return Err(crate::host::arg_value_error(
            vm,
            a,
            n + 1,
            "must not contain any null bytes",
        ));
    }
    Ok(())
}

fn create_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = sqlite_dbh(vm, a)?;
    let name = a.str(0);
    let cb = a.at(1);
    let argc = if a.has(2) { a.int(2) } else { -1 };
    let flags = a.int(3);
    Ok(Value::Bool(h.create_function(
        name.as_bytes(),
        cb,
        argc,
        flags,
    )))
}

fn create_aggregate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = sqlite_dbh(vm, a)?;
    let name = a.str(0);
    let step = a.at(1);
    let fini = a.at(2);
    let argc = if a.has(3) { a.int(3) } else { -1 };
    Ok(Value::Bool(h.create_aggregate(
        name.as_bytes(),
        step,
        fini,
        argc,
    )))
}

fn create_collation(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = sqlite_dbh(vm, a)?;
    let name = a.str(0);
    let cb = bind_callable(vm, a.at(1));
    Ok(Value::Bool(h.create_collation(name.as_bytes(), cb)))
}

fn load_extension(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    check_path(vm, a, 0, &name)?;
    if name.is_empty() {
        return Err(crate::host::arg_value_error(vm, a, 1, "must not be empty"));
    }
    let h = sqlite_dbh(vm, a)?;
    with_sqlite(vm, |host| {
        let mut cx = php_sqlite::Cx::new(host);
        let r = h.load_extension(&mut cx, name.as_bytes());
        match r {
            Ok(()) => cx.finish(()),
            Err(e) => Err(e),
        }
    })?;
    Ok(Value::Null)
}

fn open_blob(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = sqlite_dbh(vm, a)?;
    let table = a.str(0);
    check_path(vm, a, 0, &table)?;
    let column = a.str(1);
    check_path(vm, a, 1, &column)?;
    let rowid = a.int(2);
    let dbname = a.opt(3).map_or_else(|| ZStr::from("main"), |_| a.str(3));
    check_path(vm, a, 3, &dbname)?;
    let flags = if a.has(4) {
        a.int(4)
    } else {
        php_sqlite::constants::SQLITE3_OPEN_READONLY
    };
    let blob = with_sqlite(vm, |host| {
        let mut cx = php_sqlite::Cx::new(host);
        let b = h.open_blob(
            &mut cx,
            table.as_bytes(),
            column.as_bytes(),
            rowid,
            dbname.as_bytes(),
            flags,
        );
        cx.finish(b)
    })?;
    Ok(match blob {
        Some(b) => crate::streams::new_stream(b.into_stream()),
        None => Value::Bool(false),
    })
}

fn set_authorizer(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = sqlite_dbh(vm, a)?;
    let cb = a.opt(0).map(|cb| bind_callable(vm, cb));
    h.set_authorizer(cb);
    Ok(Value::Null)
}

/// The legacy driver methods callable on any `PDO` connected to `SQLite`
/// (`dbh_method_get` + `ZEND_ACC_DEPRECATED`).
fn legacy(vm: &mut Vm, a: &Args, name: &str, replacement: &str) -> VmResult<PdoSqliteDbh> {
    let (o, p) = this_pdo(a);
    let Some(h) = p.sqlite() else {
        return Err(vm.throw(
            "Error",
            &format!(
                "Call to undefined method {}::{name}()",
                o.class_name().to_string_lossy()
            ),
        ));
    };
    vm.raise(
        php_runtime::ErrorLevel::Deprecated.bits(),
        &format!("Method PDO::{name}() is deprecated since 8.5, use {replacement}() instead"),
    )?;
    Ok(h)
}

/// `Z_PARAM_FUNC` of the legacy methods (checked after the deprecation,
/// like the engine does for `ZEND_ACC_DEPRECATED` functions).
fn legacy_callable(vm: &mut Vm, a: &Args, n: usize, method: &str) -> VmResult<Value> {
    let cb = a.at(n);
    if vm.is_callable(&cb) {
        return Ok(bind_callable(vm, cb));
    }
    let name = a
        .func
        .as_ref()
        .and_then(|f| f.params.get(n))
        .map_or_else(String::new, |p| p.name.to_string_lossy());
    let why = vm.callable_error_message(&cb);
    Err(vm.throw(
        "TypeError",
        &format!(
            "PDO::{method}(): Argument #{} (${name}) must be a valid callback, {why}",
            n + 1
        ),
    ))
}

fn sqlite_create_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = "sqliteCreateFunction";
    let h = legacy(vm, a, m, "Pdo\\Sqlite::createFunction")?;
    let cb = legacy_callable(vm, a, 1, m)?;
    let name = a.str(0);
    let argc = if a.has(2) { a.int(2) } else { -1 };
    Ok(Value::Bool(h.create_function(
        name.as_bytes(),
        cb,
        argc,
        a.int(3),
    )))
}

fn sqlite_create_aggregate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = "sqliteCreateAggregate";
    let h = legacy(vm, a, m, "Pdo\\Sqlite::createAggregate")?;
    let step = legacy_callable(vm, a, 1, m)?;
    let fini = legacy_callable(vm, a, 2, m)?;
    let name = a.str(0);
    let argc = if a.has(3) { a.int(3) } else { -1 };
    Ok(Value::Bool(h.create_aggregate(
        name.as_bytes(),
        step,
        fini,
        argc,
    )))
}

fn sqlite_create_collation(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = "sqliteCreateCollation";
    let h = legacy(vm, a, m, "Pdo\\Sqlite::createCollation")?;
    let cb = legacy_callable(vm, a, 1, m)?;
    let name = a.str(0);
    Ok(Value::Bool(h.create_collation(name.as_bytes(), cb)))
}

fn const_value(v: ConstValue) -> Value {
    match v {
        ConstValue::Int(i) => Value::Int(i),
        ConstValue::Str(s) => Value::from(s),
    }
}

/// `dbh_get_gc`: the default statement constructor arguments.
fn pdo_gc(p: &Pdo, v: &mut php_vm::gc::GcVisitor<'_>) {
    for x in p.gc_values() {
        v.value(&x);
    }
}

/// Registers `PDOException`, `PDO`, `Pdo\Sqlite` and `pdo_drivers()`.
#[allow(clippy::too_many_lines)] // one declarative class table
pub fn register(vm: &mut Vm) {
    php_vm::gc::register_traverse_with::<Pdo>(pdo_gc);
    ClassBuilder::new("PDOException")
        .extends(vm, "RuntimeException")
        .prop("code", acc::PROTECTED, None, Some(Value::Int(0)))
        .prop("errorInfo", acc::PUBLIC, Some("?array"), Some(Value::Null))
        .register(vm);

    let mut pdo = ClassBuilder::new("PDO")
        .flags(class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE)
        .hooks(ClassHooks {
            create: Some(pdo_create),
            ..Default::default()
        });
    for (name, v) in pdo_class_constants() {
        pdo = pdo.constant(name, const_value(v));
    }
    for &(name, new, v) in php_sqlite::constants::PDO_SQLITE_DEPRECATED_ALIASES {
        let msg = format!("use Pdo\\Sqlite::{new} instead");
        pdo = pdo.deprecated_constant(name, Value::Int(v), "8.5", &msg);
    }
    let pub_ = acc::PUBLIC;
    pdo.method(
        "__construct(string $dsn, ?string $username = null, #[\\SensitiveParameter] ?string $password = null, ?array $options = null)",
        pub_,
        construct,
    )
    .method(
        "connect(string $dsn, ?string $username = null, #[\\SensitiveParameter] ?string $password = null, ?array $options = null): static",
        pub_ | acc::STATIC,
        connect,
    )
    .method("beginTransaction(): bool", pub_, begin_transaction)
    .method("commit(): bool", pub_, commit)
    .method("errorCode(): ?string", pub_, error_code)
    .method("errorInfo(): array", pub_, error_info)
    .method("exec(string $statement): int|false", pub_, exec)
    .method("getAttribute(int $attribute): mixed", pub_, get_attribute)
    .method(
        "getAvailableDrivers(): array",
        pub_ | acc::STATIC,
        get_available_drivers,
    )
    .method("inTransaction(): bool", pub_, in_transaction)
    .method(
        "lastInsertId(?string $name = null): string|false",
        pub_,
        last_insert_id,
    )
    .method(
        "prepare(string $query, array $options = []): PDOStatement|false",
        pub_,
        prepare,
    )
    .method(
        "query(string $query, ?int $fetchMode = null, mixed ...$fetchModeArgs): PDOStatement|false",
        pub_,
        query,
    )
    .method(
        "quote(string $string, int $type = PDO::PARAM_STR): string|false",
        pub_,
        quote,
    )
    .method("rollBack(): bool", pub_, roll_back)
    .method(
        "setAttribute(int $attribute, mixed $value): bool",
        pub_,
        set_attribute,
    )
    .method(
        "sqliteCreateFunction(string $name, mixed $callback, int $numArgs = -1, int $flags = 0): bool",
        pub_,
        sqlite_create_function,
    )
    .method(
        "sqliteCreateAggregate(string $name, mixed $step, mixed $finalize, int $numArgs = -1): bool",
        pub_,
        sqlite_create_aggregate,
    )
    .method(
        "sqliteCreateCollation(string $name, mixed $callback): bool",
        pub_,
        sqlite_create_collation,
    )
    .register(vm);

    let mut sq = ClassBuilder::new("Pdo\\Sqlite")
        .extends(vm, "PDO")
        .flags(class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE);
    for &(name, v) in php_sqlite::constants::PDO_SQLITE_CLASS_CONSTANTS {
        sq = sq.constant(name, Value::Int(v));
    }
    sq.method(
        "createAggregate(string $name, callable $step, callable $finalize, int $numArgs = -1): bool",
        pub_,
        create_aggregate,
    )
    .method(
        "createCollation(string $name, callable $callback): bool",
        pub_,
        create_collation,
    )
    .method(
        "createFunction(string $function_name, callable $callback, int $num_args = -1, int $flags = 0): bool",
        pub_,
        create_function,
    )
    .method("loadExtension(string $name): void", pub_, load_extension)
    .method(
        "openBlob(string $table, string $column, int $rowid, ?string $dbname = \"main\", int $flags = \\Pdo\\Sqlite::OPEN_READONLY)",
        pub_,
        open_blob,
    )
    .method("setAuthorizer(?callable $callback): void", pub_, set_authorizer)
    .register(vm);

    vm.register_function("pdo_drivers(): array", pdo_drivers);
}

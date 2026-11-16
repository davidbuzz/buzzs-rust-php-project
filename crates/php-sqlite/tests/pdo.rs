// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of the `ext/pdo_sqlite/tests/*.phpt` expectations at the driver
//! level. A tiny stand-in for the PDO core (`Core`) drives the driver the
//! way `pdo_stmt.c` / `pdo_dbh.c` do.

#![allow(
    clippy::format_push_string,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

mod common;

use common::{dump, s, throws_object, TestHost};
use php_runtime::{PhpRef, Resource, RuntimeError, Value, ZArray, ZStr};
use php_sqlite::constants::*;
use php_sqlite::pdo::{
    ConnectError, HookError, ParamEvent, PdoBoundParam, PdoSqliteDbh, PdoSqliteStmt, SqlState,
    ATTR_CLIENT_VERSION, ATTR_SERVER_VERSION, ATTR_TIMEOUT, ERRMODE_EXCEPTION, ERRMODE_SILENT,
    ERRMODE_WARNING, PARAM_INT, PARAM_LOB, PARAM_STR,
};
use php_sqlite::{Cx, PhpError};

fn connect(
    h: &mut TestHost,
    dsn: &str,
    options: Option<&ZArray>,
) -> Result<PdoSqliteDbh, ConnectError> {
    let (driver, ds) = PdoSqliteDbh::split_dsn(dsn.as_bytes()).unwrap();
    assert_eq!(driver, b"sqlite");
    let mut cx = Cx::new(h);
    let r = PdoSqliteDbh::connect(&mut cx, ds, options);
    cx.finish(()).unwrap();
    r
}

fn mem(h: &mut TestHost) -> PdoSqliteDbh {
    connect(h, "sqlite::memory:", None).expect("connect")
}

fn exec(h: &mut TestHost, db: &PdoSqliteDbh, sql: &str) -> Result<i64, SqlState> {
    let mut cx = Cx::new(h);
    let r = db.exec(&mut cx, sql.as_bytes());
    cx.finish(()).unwrap();
    r
}

fn err_text(e: &PhpError) -> String {
    format!(
        "{}: {}",
        e.class_name().unwrap_or_default(),
        e.message().unwrap_or_default()
    )
}

/// `pdo_handle_error()` message for the current driver error.
fn pdo_message(db: &PdoSqliteDbh, state: SqlState) -> String {
    let info = db.fetch_error();
    let desc = state.description().unwrap_or("<<Unknown error>>");
    match info.as_slice() {
        [Value::Int(code), Value::Str(msg)] => {
            format!(
                "SQLSTATE[{state}]: {desc}: {code} {}",
                msg.to_string_lossy()
            )
        }
        _ => format!("SQLSTATE[{state}]: {desc}"),
    }
}

/// A minimal PDO statement core.
struct Core {
    stmt: PdoSqliteStmt,
    executed: bool,
    params: Vec<PdoBoundParam>,
    columns: i64,
    incomplete: bool,
}

#[derive(Debug)]
enum CoreError {
    Sql(SqlState),
    Hook(HookError),
    Throw(PhpError),
}

impl Core {
    fn prepare(h: &mut TestHost, db: &PdoSqliteDbh, sql: &str) -> Result<Core, CoreError> {
        let mut cx = Cx::new(h);
        let r = db.prepare(&mut cx, sql.as_bytes(), 0);
        cx.finish(()).map_err(CoreError::Throw)?;
        Ok(Core {
            stmt: r.map_err(CoreError::Sql)?,
            executed: false,
            params: vec![],
            columns: 0,
            incomplete: false,
        })
    }

    fn bind(&mut self, key: Result<i64, &str>, value: Value, param_type: i64) {
        let (paramno, name) = match key {
            Ok(n) => (n, None),
            Err(n) => (
                -1,
                Some(ZStr::from(if n.starts_with(':') {
                    n.to_string()
                } else {
                    format!(":{n}")
                })),
            ),
        };
        self.params
            .retain(|p| !(p.name == name && p.paramno == paramno));
        self.params.push(PdoBoundParam {
            parameter: value,
            paramno,
            name,
            param_type,
            is_param: true,
        });
    }

    /// `PDOStatement::execute()`.
    fn execute(
        &mut self,
        h: &mut TestHost,
        input: Option<Vec<(Result<i64, &str>, Value)>>,
    ) -> Result<(), CoreError> {
        if self.executed {
            self.stmt.cursor_closer();
        }
        self.incomplete = true;
        let mut cx = Cx::new(h);
        if let Some(input) = input {
            self.params.clear();
            for (k, v) in input {
                // really_register_bound_param(): PARAM_STR values are
                // converted to strings at bind time.
                let v = if v.is_null() {
                    v
                } else {
                    let r = v.to_zstr(cx.host());
                    match cx.check(r) {
                        Some(s) => Value::Str(s),
                        None => return Err(CoreError::Throw(cx.take_exception().unwrap())),
                    }
                };
                self.bind(k, v, PARAM_STR);
            }
        }
        for p in &mut self.params {
            if let Err(e) = self
                .stmt
                .param_hook(&mut cx, p, ParamEvent::ExecPre, self.executed)
            {
                if let Some(x) = cx.take_exception() {
                    return Err(CoreError::Throw(x));
                }
                return Err(CoreError::Hook(e));
            }
        }
        let r = self.stmt.execute(&mut cx, self.executed);
        if let Some(x) = cx.take_exception() {
            return Err(CoreError::Throw(x));
        }
        let info = r.map_err(CoreError::Sql)?;
        self.columns = info.column_count;
        self.executed = true;
        self.incomplete = false;
        Ok(())
    }

    /// `fetch(PDO::FETCH_NUM)`: `None` when exhausted.
    fn fetch(&mut self, h: &mut TestHost) -> Result<Option<Vec<Value>>, CoreError> {
        if !self.executed || self.incomplete {
            return Ok(None);
        }
        let mut cx = Cx::new(h);
        let r = self.stmt.fetch(&mut cx);
        if let Some(x) = cx.take_exception() {
            return Err(CoreError::Throw(x));
        }
        if !r.map_err(CoreError::Sql)? {
            return Ok(None);
        }
        let row = (0..self.columns)
            .map(|c| self.stmt.get_col(c).map_err(CoreError::Sql))
            .collect::<Result<_, _>>()?;
        Ok(Some(row))
    }

    fn fetch_all(&mut self, h: &mut TestHost) -> Result<Vec<Vec<Value>>, CoreError> {
        let mut out = vec![];
        while let Some(r) = self.fetch(h)? {
            out.push(r);
        }
        Ok(out)
    }

    fn names(&self) -> Vec<String> {
        (0..self.columns)
            .map(|c| self.stmt.describe(c).unwrap().name.to_string_lossy())
            .collect()
    }
}

fn query(h: &mut TestHost, db: &PdoSqliteDbh, sql: &str) -> Result<Core, CoreError> {
    let mut c = Core::prepare(h, db, sql)?;
    c.execute(h, None)?;
    Ok(c)
}

fn col0(rows: &[Vec<Value>]) -> String {
    rows.iter().map(|r| dump(&r[0])).collect()
}

#[test]
fn bug38334_native_types_and_strings() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE test_38334 (i INTEGER , f DOUBLE, s VARCHAR(255))",
    )
    .unwrap();
    exec(
        &mut h,
        &db,
        "INSERT INTO test_38334 VALUES (42, 46.7, 'test')",
    )
    .unwrap();
    let mut q = query(&mut h, &db, "SELECT * FROM test_38334").unwrap();
    assert_eq!(q.names(), ["i", "f", "s"]);
    let row = q.fetch(&mut h).unwrap().unwrap();
    assert_eq!(
        row.iter().map(dump).collect::<String>(),
        "int(42)\nfloat(46.7)\nstring(4) \"test\"\n"
    );
    exec(
        &mut h,
        &db,
        "INSERT INTO test_38334 VALUES (10000000000, 0.0, '')",
    )
    .unwrap();
    let mut q = query(&mut h, &db, "SELECT i FROM test_38334 WHERE f = 0.0").unwrap();
    assert_eq!(
        dump(&q.fetch(&mut h).unwrap().unwrap()[0]),
        "int(10000000000)\n"
    );
    exec(
        &mut h,
        &db,
        "INSERT INTO test_38334 VALUES ('test', 'test', 'x')",
    )
    .unwrap();
    let mut q = query(&mut h, &db, "SELECT * FROM test_38334 WHERE s = 'x'").unwrap();
    assert_eq!(
        q.fetch(&mut h)
            .unwrap()
            .unwrap()
            .iter()
            .map(dump)
            .collect::<String>(),
        "string(4) \"test\"\nstring(4) \"test\"\nstring(1) \"x\"\n"
    );
}

#[test]
fn bug33841_row_count_and_bind_param() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test_33841 (text)").unwrap();
    let mut st = Core::prepare(&mut h, &db, "INSERT INTO test_33841 VALUES ( :text )").unwrap();
    let name = PhpRef::new(Value::Null);
    st.bind(Err(":text"), Value::Reference(name.clone()), PARAM_STR);
    name.set(s("test1"));
    let mut cx = Cx::new(&mut h);
    st.stmt
        .param_hook(&mut cx, &mut st.params[0], ParamEvent::ExecPre, false)
        .unwrap();
    let info = st.stmt.execute(&mut cx, false).unwrap();
    assert_eq!(info.row_count, Some(1));
    assert_eq!(info.column_count, 0);
    drop(cx);
    let mut st = Core::prepare(&mut h, &db, "UPDATE test_33841 SET text = :text ").unwrap();
    st.bind(Err("text"), Value::Reference(name.clone()), PARAM_STR);
    name.set(s("test2"));
    let mut cx = Cx::new(&mut h);
    st.stmt
        .param_hook(&mut cx, &mut st.params[0], ParamEvent::ExecPre, false)
        .unwrap();
    assert_eq!(st.stmt.execute(&mut cx, false).unwrap().row_count, Some(1));
    // gh9032-style statement sql
    assert_eq!(
        st.stmt.sql().to_string_lossy(),
        "UPDATE test_33841 SET text = :text "
    );
}

#[test]
fn errors_sqlstates_and_extended_codes() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE dog ( id INTEGER PRIMARY KEY, name TEXT, annoying INTEGER )",
    )
    .unwrap();
    exec(&mut h, &db, "INSERT INTO dog VALUES (1, 'Annoying Dog', 1)").unwrap();
    assert!(db.fetch_error().is_empty());
    let st = exec(&mut h, &db, "INSERT INTO dog VALUES (1, 'Annoying Dog', 1)").unwrap_err();
    assert_eq!(st.as_str(), "23000");
    assert_eq!(dump(&db.fetch_error()[0]), "int(19)\n");
    assert_eq!(
        pdo_message(&db, st),
        "SQLSTATE[23000]: Integrity constraint violation: 19 UNIQUE constraint failed: dog.id"
    );
    // extended result codes as a driver option
    let mut opts = php_runtime::HashTable::new();
    opts.insert(
        php_runtime::ArrayKey::Int(PDO_SQLITE_ATTR_EXTENDED_RESULT_CODES),
        Value::Bool(true),
    );
    let db = connect(&mut h, "sqlite::memory:", Some(&ZArray::from_table(opts))).unwrap();
    let mut cx = Cx::new(&mut h);
    assert!(db.set_attribute(
        &mut cx,
        PDO_SQLITE_ATTR_EXTENDED_RESULT_CODES,
        &Value::Bool(true)
    ));
    drop(cx);
    exec(
        &mut h,
        &db,
        "CREATE TABLE dog ( id INTEGER PRIMARY KEY, name TEXT, annoying INTEGER )",
    )
    .unwrap();
    exec(&mut h, &db, "INSERT INTO dog VALUES (1, 'Annoying Dog', 1)").unwrap();
    let st = exec(&mut h, &db, "INSERT INTO dog VALUES (1, 'Annoying Dog', 1)").unwrap_err();
    assert_eq!(st.as_str(), "HY000");
    assert_eq!(dump(&db.fetch_error()[0]), "int(1555)\n");
    // syntax errors
    let st = exec(&mut h, &db, "SELEC 1").unwrap_err();
    assert_eq!(
        pdo_message(&db, st),
        "SQLSTATE[HY000]: General error: 1 near \"SELEC\": syntax error"
    );
    let Err(CoreError::Sql(st)) = Core::prepare(&mut h, &db, "SELECT * FROM nope") else {
        panic!()
    };
    assert_eq!(
        pdo_message(&db, st),
        "SQLSTATE[HY000]: General error: 1 no such table: nope"
    );
    // scrollable cursors are refused
    let mut cx = Cx::new(&mut h);
    assert!(db.prepare(&mut cx, b"SELECT 1", 1).is_err());
}

#[test]
fn gh24990_failed_execute_then_continue() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE test (k TEXT PRIMARY KEY NOT NULL)",
    )
    .unwrap();
    exec(&mut h, &db, "INSERT INTO test VALUES ('a')").unwrap();
    let mut st = Core::prepare(&mut h, &db, "INSERT INTO test (k) VALUES (?)").unwrap();
    let mut out = vec![];
    for k in ["a", "b", "b", "c"] {
        match st.execute(&mut h, Some(vec![(Ok(0), s(k))])) {
            Ok(()) => out.push("bool(true)".to_string()),
            Err(CoreError::Sql(state)) => out.push(pdo_message(&db, state)),
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(
        out,
        [
            "SQLSTATE[23000]: Integrity constraint violation: 19 UNIQUE constraint failed: test.k",
            "bool(true)",
            "SQLSTATE[23000]: Integrity constraint violation: 19 UNIQUE constraint failed: test.k",
            "bool(true)",
        ]
    );
    let mut q = query(&mut h, &db, "SELECT k FROM test ORDER BY k").unwrap();
    assert_eq!(
        col0(&q.fetch_all(&mut h).unwrap()),
        "string(1) \"a\"\nstring(1) \"b\"\nstring(1) \"c\"\n"
    );
}

#[test]
fn pdo_sqlite_failed_execute() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let mut st = Core::prepare(&mut h, &db, "SELECT ? AS value UNION ALL SELECT 2").unwrap();
    st.execute(&mut h, Some(vec![(Ok(0), s("first"))])).unwrap();
    assert_eq!(
        dump(&st.fetch(&mut h).unwrap().unwrap()[0]),
        "string(5) \"first\"\n"
    );
    let obj = Value::Object(php_runtime::PlainObject::create("stdClass", ZArray::new()));
    let Err(CoreError::Throw(e)) = st.execute(&mut h, Some(vec![(Ok(0), obj)])) else {
        panic!()
    };
    assert_eq!(
        err_text(&e),
        "Error: Object of class stdClass could not be converted to string"
    );
    assert!(st.fetch(&mut h).unwrap().is_none());
    st.execute(&mut h, Some(vec![(Ok(0), s("third"))])).unwrap();
    assert_eq!(
        col0(&st.fetch_all(&mut h).unwrap()),
        "string(5) \"third\"\nint(2)\n"
    );
    // a UDF throwing during execute
    let f = h.func("fail_execute", |_, a| {
        if a[0].as_str().is_some_and(|s| s.as_bytes() == b"1") {
            return Err(RuntimeError::Custom {
                class: ZStr::from("RuntimeException"),
                message: "execution failed".into(),
            });
        }
        Ok(s("first"))
    });
    assert!(db.create_function(b"fail_execute", f, -1, 0));
    let mut st = Core::prepare(
        &mut h,
        &db,
        "SELECT fail_execute(?) AS value UNION ALL SELECT 2",
    )
    .unwrap();
    st.execute(&mut h, Some(vec![(Ok(0), Value::from(""))]))
        .unwrap();
    assert_eq!(
        dump(&st.fetch(&mut h).unwrap().unwrap()[0]),
        "string(5) \"first\"\n"
    );
    let Err(CoreError::Throw(e)) = st.execute(&mut h, Some(vec![(Ok(0), Value::from("1"))])) else {
        panic!()
    };
    assert_eq!(err_text(&e), "RuntimeException: execution failed");
    assert!(st.fetch(&mut h).unwrap().is_none());
    st.execute(&mut h, Some(vec![(Ok(0), Value::from(""))]))
        .unwrap();
    assert_eq!(
        col0(&st.fetch_all(&mut h).unwrap()),
        "string(5) \"first\"\nint(2)\n"
    );
}

#[test]
fn quote_rules_gh13952() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let cases: [&[u8]; 8] = [
        b"",
        b"x",
        b"foobar",
        b"foo'''bar",
        b"'foo'''bar'",
        b"\x80",
        b"a\0b",
        b"\0",
    ];
    for mode in [ERRMODE_EXCEPTION, ERRMODE_WARNING, ERRMODE_SILENT] {
        for c in cases {
            let mut cx = Cx::new(&mut h);
            let q = db.quote(&mut cx, c, PARAM_STR, mode);
            let pending = cx.take_exception();
            drop(cx);
            if c.contains(&0) {
                assert!(q.is_none());
                match mode {
                    ERRMODE_EXCEPTION => assert_eq!(
                        err_text(&pending.unwrap()),
                        "PDOException: SQLite PDO::quote does not support null bytes"
                    ),
                    ERRMODE_WARNING => {
                        assert_eq!(
                            h.take_log(),
                            ["Warning: SQLite PDO::quote does not support null bytes"]
                        );
                    }
                    _ => assert!(h.take_log().is_empty() && pending.is_none()),
                }
            } else {
                let q = q.unwrap();
                let mut st = query(
                    &mut h,
                    &db,
                    &format!("SELECT {}", String::from_utf8_lossy(q.as_bytes())),
                )
                .unwrap();
                let v = st.fetch(&mut h).unwrap().unwrap().remove(0);
                assert_eq!(
                    v.as_str().unwrap().as_bytes(),
                    if c == b"\x80" {
                        "\u{fffd}".as_bytes()
                    } else {
                        c
                    }
                );
            }
        }
    }
    let mut cx = Cx::new(&mut h);
    assert_eq!(
        db.quote(&mut cx, b"it's", PARAM_STR, ERRMODE_SILENT)
            .unwrap()
            .as_bytes(),
        b"'it''s'"
    );
}

#[test]
fn last_insert_id_transactions_and_attributes() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE t (id INT AUTO INCREMENT, name TEXT)",
    )
    .unwrap();
    exec(
        &mut h,
        &db,
        "INSERT INTO t VALUES (NULL, 'PHP'), (NULL, 'PHP6')",
    )
    .unwrap();
    assert_eq!(db.last_insert_id().as_bytes(), b"2");
    // bug81227
    assert!(!db.in_transaction());
    exec(&mut h, &db, "BEGIN EXCLUSIVE TRANSACTION").unwrap();
    assert!(db.in_transaction());
    let mut cx = Cx::new(&mut h);
    db.commit(&mut cx).unwrap();
    assert!(!db.in_transaction());
    db.begin(&mut cx).unwrap();
    assert!(db.in_transaction());
    db.rollback(&mut cx).unwrap();
    assert_eq!(db.rollback(&mut cx).unwrap_err().as_str(), "HY000");
    drop(cx);
    // attributes
    let v = db.get_attribute(ATTR_SERVER_VERSION).unwrap();
    assert_eq!(
        v.as_str().unwrap().to_string_lossy(),
        php_sqlite::sqlite_version()
    );
    assert!(db.get_attribute(ATTR_CLIENT_VERSION).is_some());
    assert!(db.get_attribute(123_456).is_none());
    assert_eq!(
        dump(&db.get_attribute(PDO_SQLITE_ATTR_TRANSACTION_MODE).unwrap()),
        "int(0)\n"
    );
    let mut cx = Cx::new(&mut h);
    for (v, ok) in [(0, true), (1, true), (2, true), (4, false)] {
        assert_eq!(
            db.set_attribute(&mut cx, PDO_SQLITE_ATTR_TRANSACTION_MODE, &Value::Int(v)),
            ok
        );
    }
    assert_eq!(
        dump(&db.get_attribute(PDO_SQLITE_ATTR_TRANSACTION_MODE).unwrap()),
        "int(2)\n"
    );
    assert!(db.set_attribute(&mut cx, ATTR_TIMEOUT, &s("5")));
    assert!(!db.set_attribute(&mut cx, ATTR_TIMEOUT, &s("nonsense")));
    assert_eq!(
        err_text(&cx.take_exception().unwrap()),
        "TypeError: Attribute value must be of type int for selected attribute, string given"
    );
    assert!(!db.set_attribute(&mut cx, ATTR_TIMEOUT, &Value::Null));
    assert_eq!(
        err_text(&cx.take_exception().unwrap()),
        "TypeError: Attribute value must be of type int for selected attribute, null given"
    );
    assert!(!db.set_attribute(&mut cx, 99, &Value::Int(1)));
}

#[test]
fn transaction_mode_locking() {
    let mut h = TestHost::new();
    let dsn = format!(
        "sqlite:file:phpsqlite{}?mode=memory&cache=shared",
        std::process::id()
    );
    let a = connect(&mut h, &dsn, None).unwrap();
    let b = connect(&mut h, &dsn, None).unwrap();
    let mut cx = Cx::new(&mut h);
    a.begin(&mut cx).unwrap();
    drop(cx);
    exec(&mut h, &b, "begin immediate transaction").unwrap();
    exec(&mut h, &b, "rollback").unwrap();
    let mut cx = Cx::new(&mut h);
    a.rollback(&mut cx).unwrap();
    assert!(a.set_attribute(
        &mut cx,
        PDO_SQLITE_ATTR_TRANSACTION_MODE,
        &Value::Int(PDO_SQLITE_TRANSACTION_MODE_IMMEDIATE)
    ));
    a.begin(&mut cx).unwrap();
    drop(cx);
    let st = exec(&mut h, &b, "begin immediate transaction").unwrap_err();
    assert_eq!(
        pdo_message(&b, st),
        "SQLSTATE[HY000]: General error: 6 database table is locked"
    );
}

#[test]
fn column_meta() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    // bug79664: no rows
    let q = query(&mut h, &db, "select 1 where 0").unwrap();
    assert_eq!(q.columns, 1);
    assert_eq!(
        dump(&Value::Array(q.stmt.column_meta(0, true).unwrap())),
        "array(3) {\n  [\"native_type\"]=>\n  string(4) \"null\"\n  [\"pdo_type\"]=>\n  int(0)\n  [\"flags\"]=>\n  array(0) {\n  }\n}\n"
    );
    assert_eq!(q.stmt.describe(0).unwrap().maxlen, -1);
    // gh17837: not executed
    let p = Core::prepare(&mut h, &db, "select :a, :b, ?").unwrap();
    assert!(p.stmt.column_meta(0, false).unwrap_err().is_none());
    // bug_42589: table names; declared types; blob flag
    exec(
        &mut h,
        &db,
        "CREATE TABLE test_42589 (field1 VARCHAR(10), b BLOB, f REAL)",
    )
    .unwrap();
    exec(
        &mut h,
        &db,
        "INSERT INTO test_42589 VALUES('test', x'00', 1.5)",
    )
    .unwrap();
    let mut q = query(
        &mut h,
        &db,
        "SELECT * FROM test_42589 t1 LEFT JOIN test_42589 t2 ON t1.field1 = t2.field1",
    )
    .unwrap();
    let m0 = q.stmt.column_meta(0, true).unwrap();
    assert_eq!(
        m0.get_str(b"table").unwrap().as_str().unwrap().as_bytes(),
        b"test_42589"
    );
    assert_eq!(
        m0.get_str(b"sqlite:decl_type")
            .unwrap()
            .as_str()
            .unwrap()
            .as_bytes(),
        b"VARCHAR(10)"
    );
    assert_eq!(
        m0.get_str(b"native_type")
            .unwrap()
            .as_str()
            .unwrap()
            .as_bytes(),
        b"string"
    );
    let m3 = q.stmt.column_meta(3, true).unwrap();
    assert_eq!(
        m3.get_str(b"table").unwrap().as_str().unwrap().as_bytes(),
        b"test_42589"
    );
    let m1 = q.stmt.column_meta(1, true).unwrap();
    assert_eq!(
        dump(m1.get_str(b"flags").unwrap()),
        "array(1) {\n  [0]=>\n  string(4) \"blob\"\n}\n"
    );
    let m2 = q.stmt.column_meta(2, true).unwrap();
    assert_eq!(
        m2.get_str(b"native_type")
            .unwrap()
            .as_str()
            .unwrap()
            .as_bytes(),
        b"double"
    );
    assert!(q.stmt.column_meta(99, true).unwrap_err().is_some());
    q.fetch_all(&mut h).unwrap();
}

#[test]
fn statement_attributes_and_explain() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let st = Core::prepare(&mut h, &db, "SELECT 1;").unwrap();
    assert_eq!(
        dump(
            &st.stmt
                .get_attribute(PDO_SQLITE_ATTR_READONLY_STATEMENT)
                .unwrap()
        ),
        "bool(true)\n"
    );
    let st = Core::prepare(&mut h, &db, "CREATE TABLE test_busy (a string);").unwrap();
    assert_eq!(
        dump(
            &st.stmt
                .get_attribute(PDO_SQLITE_ATTR_READONLY_STATEMENT)
                .unwrap()
        ),
        "bool(false)\n"
    );
    exec(&mut h, &db, "CREATE TABLE test_busy (a string); INSERT INTO test_busy VALUES ('interleaved'), ('statements')").unwrap();
    let mut st = Core::prepare(&mut h, &db, "SELECT a FROM test_busy").unwrap();
    assert_eq!(
        dump(
            &st.stmt
                .get_attribute(PDO_SQLITE_ATTR_BUSY_STATEMENT)
                .unwrap()
        ),
        "bool(false)\n"
    );
    st.execute(&mut h, None).unwrap();
    assert_eq!(
        dump(
            &st.stmt
                .get_attribute(PDO_SQLITE_ATTR_BUSY_STATEMENT)
                .unwrap()
        ),
        "bool(true)\n"
    );
    assert!(st.stmt.get_attribute(1).is_none());
    // explain
    let st = Core::prepare(&mut h, &db, "SELECT * FROM test_busy").unwrap();
    let mut cx = Cx::new(&mut h);
    assert!(st
        .stmt
        .set_attribute(&mut cx, PDO_SQLITE_ATTR_EXPLAIN_STATEMENT, &Value::Int(2)));
    assert_eq!(
        dump(
            &st.stmt
                .get_attribute(PDO_SQLITE_ATTR_EXPLAIN_STATEMENT)
                .unwrap()
        ),
        "int(2)\n"
    );
    let cases = [
        (
            s("EXPLAIN"),
            "TypeError: explain mode must be of type int, string given",
        ),
        (
            Value::Int(-1),
            "ValueError: explain mode must be one of the Pdo\\Sqlite::EXPLAIN_MODE_* constants",
        ),
        (
            Value::Int(256),
            "ValueError: explain mode must be one of the Pdo\\Sqlite::EXPLAIN_MODE_* constants",
        ),
    ];
    for (v, msg) in cases {
        assert!(!st
            .stmt
            .set_attribute(&mut cx, PDO_SQLITE_ATTR_EXPLAIN_STATEMENT, &v));
        assert_eq!(err_text(&cx.take_exception().unwrap()), msg);
    }
    assert!(!st.stmt.set_attribute(&mut cx, 1, &Value::Int(0)));
    drop(cx);
    let mut st = st;
    st.execute(&mut h, None).unwrap();
    let rows = st.fetch_all(&mut h).unwrap();
    assert_eq!(dump(&rows[0][3]), "string(14) \"SCAN test_busy\"\n");
}

#[test]
fn user_functions_aggregates_collations() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE t (id INT AUTO INCREMENT, name TEXT)",
    )
    .unwrap();
    exec(
        &mut h,
        &db,
        "INSERT INTO t VALUES (NULL, 'PHP'), (NULL, 'PHP6')",
    )
    .unwrap();
    let lower = h.func("lower", |_, a| {
        Ok(Value::Str(a[0].as_str().unwrap().to_ascii_lowercase()))
    });
    assert!(db.create_function(b"testing", lower, 1, 2048));
    let mut q = query(&mut h, &db, "SELECT testing(name) FROM t").unwrap();
    assert_eq!(q.names(), ["testing(name)"]);
    assert_eq!(
        col0(&q.fetch_all(&mut h).unwrap()),
        "string(3) \"php\"\nstring(4) \"php6\"\n"
    );
    // pdo_sqlite_createaggregate: the context is passed by reference and the
    // row counter starts at 1 (and is incremented for the final call too)
    let step = h.func("step", |_, a| {
        let Value::Reference(r) = &a[0] else {
            panic!("context must be a reference")
        };
        let mut cur = r
            .get()
            .as_str()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default();
        cur.extend_from_slice(a[1].as_int().unwrap().to_string().as_bytes());
        r.set(Value::Str(ZStr::from(cur.clone())));
        Ok(Value::Str(ZStr::from(cur)))
    });
    let fin = h.func("fin", |h, a| {
        h.log.push(format!("final rows={}", a[1].as_int().unwrap()));
        Ok(a[0].deref())
    });
    assert!(db.create_aggregate(b"testing2", step, fin, -1));
    let mut q = query(&mut h, &db, "SELECT testing2(name) FROM t").unwrap();
    assert_eq!(col0(&q.fetch_all(&mut h).unwrap()), "string(2) \"12\"\n");
    assert_eq!(h.take_log(), ["final rows=3"]);
    // collation
    exec(
        &mut h,
        &db,
        "CREATE TABLE c (name TEXT); INSERT INTO c VALUES ('1'), ('2'), ('10')",
    )
    .unwrap();
    let nat = h.func("nat", |_, a| {
        let n = |v: &Value| {
            v.as_str()
                .unwrap()
                .to_string_lossy()
                .parse::<i64>()
                .unwrap()
        };
        Ok(Value::Int((n(&a[0]) - n(&a[1])) * 100))
    });
    assert!(db.create_collation(b"MYCOLLATE", nat));
    let mut q = query(
        &mut h,
        &db,
        "SELECT name FROM c ORDER BY name COLLATE MYCOLLATE",
    )
    .unwrap();
    assert_eq!(
        col0(&q.fetch_all(&mut h).unwrap()),
        "string(1) \"1\"\nstring(1) \"2\"\nstring(2) \"10\"\n"
    );
    let mut q = query(&mut h, &db, "SELECT name FROM c ORDER BY name").unwrap();
    assert_eq!(
        col0(&q.fetch_all(&mut h).unwrap()),
        "string(1) \"1\"\nstring(2) \"10\"\nstring(1) \"2\"\n"
    );
    let bad = h.func("bad", |_, a| Ok(a[0].clone()));
    db.create_collation(b"MYCOLLATEBAD", bad);
    h.at("PDO::query");
    let Err(CoreError::Throw(e)) = query(
        &mut h,
        &db,
        "SELECT name FROM c ORDER BY name COLLATE MYCOLLATEBAD",
    ) else {
        panic!()
    };
    assert_eq!(err_text(&e), "TypeError: PDO::query(): Return value of the collation callback must be of type int, string returned");
    // gh13998: a throwing step function
    let boom = h.func("boom", |_, _| {
        Err(RuntimeError::Custom {
            class: ZStr::from("Exception"),
            message: String::new(),
        })
    });
    let noop = h.func("noop", |_, _| Ok(Value::Null));
    db.create_aggregate(b"S", boom, noop, 1);
    assert!(matches!(
        query(&mut h, &db, "SELECT S(id) FROM t"),
        Err(CoreError::Throw(_))
    ));
    db.close();
}

#[test]
fn authorizer() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let only_select = h.func("only_select", |_, a| {
        Ok(Value::Int(i64::from(a[0].as_int() != Some(21))))
    });
    db.set_authorizer(Some(only_select));
    assert!(query(&mut h, &db, "SELECT 1;").is_ok());
    let st = exec(&mut h, &db, "CREATE TABLE test (a, b);").unwrap_err();
    assert_eq!(
        pdo_message(&db, st),
        "SQLSTATE[HY000]: General error: 23 not authorized"
    );
    db.set_authorizer(None);
    assert_eq!(
        exec(
            &mut h,
            &db,
            "CREATE TABLE test (a); INSERT INTO test VALUES (42);"
        )
        .unwrap(),
        1
    );
    let names = [
        "COPY",
        "CREATE_INDEX",
        "CREATE_TABLE",
        "CREATE_TEMP_INDEX",
        "CREATE_TEMP_TABLE",
        "CREATE_TEMP_TRIGGER",
        "CREATE_TEMP_VIEW",
        "CREATE_TRIGGER",
        "CREATE_VIEW",
        "DELETE",
        "DROP_INDEX",
        "DROP_TABLE",
        "DROP_TEMP_INDEX",
        "DROP_TEMP_TABLE",
        "DROP_TEMP_TRIGGER",
        "DROP_TEMP_VIEW",
        "DROP_TRIGGER",
        "DROP_VIEW",
        "INSERT",
        "PRAGMA",
        "READ",
        "SELECT",
        "TRANSACTION",
        "UPDATE",
    ];
    let logger = h.func("logger", move |h, a| {
        let rest: Vec<String> = a[1..]
            .iter()
            .map(|v| v.as_str().map(ZStr::to_string_lossy).unwrap_or_default())
            .collect();
        h.log.push(format!(
            "{} {}",
            names[usize::try_from(a[0].as_int().unwrap()).unwrap()],
            rest.join(",")
        ));
        Ok(Value::Int(0))
    });
    db.set_authorizer(Some(logger));
    assert_eq!(
        exec(&mut h, &db, "SELECT * FROM test WHERE a = 42;").unwrap(),
        1
    );
    assert_eq!(
        h.take_log(),
        ["SELECT ,,,", "READ test,a,main,", "READ test,a,main,"]
    );
    let fail = h.func("fail", |_, _| Ok(s("FAIL")));
    db.set_authorizer(Some(fail));
    h.at("PDO::query");
    let Err(CoreError::Throw(e)) = query(&mut h, &db, "SELECT 1;") else {
        panic!()
    };
    assert_eq!(err_text(&e), "TypeError: PDO::query(): Return value of the authorizer callback must be of type int, string returned");
    let big = h.func("big", |_, _| Ok(Value::Int(4200)));
    db.set_authorizer(Some(big));
    let Err(CoreError::Throw(e)) = query(&mut h, &db, "SELECT 1;") else {
        panic!()
    };
    assert_eq!(
        err_text(&e),
        "ValueError: PDO::query(): Return value of the authorizer callback must be one of Pdo\\Sqlite::OK, Pdo\\Sqlite::DENY, or Pdo\\Sqlite::IGNORE"
    );
    // gh9032 with open_basedir: attaching a non-literal is denied
    let mut h2 = TestHost::new();
    h2.open_basedir = Some(b"/nonexistent-basedir".to_vec());
    let db = connect(&mut h2, "sqlite::memory:", None).unwrap();
    assert_eq!(
        exec(&mut h2, &db, "attach database ':memory:' AS \"db1\"").unwrap(),
        0
    );
    let Err(CoreError::Sql(st)) = Core::prepare(&mut h2, &db, "attach database :a AS \"db2\"")
    else {
        panic!()
    };
    assert_eq!(
        pdo_message(&db, st),
        "SQLSTATE[HY000]: General error: 23 not authorized"
    );
}

#[test]
fn bind_semantics() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE users (id INTEGER NOT NULL, num INTEGER NOT NULL, b BLOB, PRIMARY KEY(id))",
    )
    .unwrap();
    let mut st = Core::prepare(
        &mut h,
        &db,
        "insert into users (id, num, b) values (:id, :num, :b)",
    )
    .unwrap();
    st.bind(Err(":id"), Value::Int(1), PARAM_INT);
    st.bind(Err(":num"), Value::Int(100_004_313_234_244), PARAM_INT);
    // bug70862: a LOB stream parameter is replaced by its contents
    let f = PhpRef::new(Value::Resource(Resource::new("stream", None)));
    st.bind(Err(":b"), Value::Reference(f.clone()), PARAM_LOB);
    st.execute(&mut h, None).unwrap();
    assert_eq!(dump(&f.get()), "string(0) \"\"\n");
    let mut q = query(&mut h, &db, "SELECT num, typeof(b) FROM users").unwrap();
    let row = q.fetch(&mut h).unwrap().unwrap();
    assert_eq!(dump(&row[0]), "int(100004313234244)\n");
    assert_eq!(dump(&row[1]), "string(4) \"blob\"\n");
    // PARAM_INT converts through the reference; PARAM_BOOL too; NULL binds NULL
    let mut st = Core::prepare(&mut h, &db, "SELECT ?, ?, typeof(?), ?").unwrap();
    let v = PhpRef::new(s("12abc"));
    st.bind(Ok(0), Value::Reference(v.clone()), PARAM_INT);
    st.bind(Ok(1), Value::Bool(true), php_sqlite::pdo::PARAM_BOOL);
    st.bind(Ok(2), Value::Null, PARAM_INT);
    st.bind(Ok(3), Value::Int(5), php_sqlite::pdo::PARAM_NULL);
    st.execute(&mut h, None).unwrap();
    assert_eq!(dump(&v.get()), "int(12)\n");
    let row = st.fetch(&mut h).unwrap().unwrap();
    assert_eq!(
        row.iter().map(dump).collect::<String>(),
        "int(12)\nint(1)\nstring(4) \"null\"\nNULL\n"
    );
    // a non-stream resource as LOB
    let mut st = Core::prepare(&mut h, &db, "SELECT ?").unwrap();
    st.bind(Ok(0), Value::Resource(Resource::new("gd", None)), PARAM_LOB);
    assert!(matches!(
        st.execute(&mut h, None),
        Err(CoreError::Hook(HookError::Impl(
            "HY105",
            "Expected a stream resource"
        )))
    ));
    // PARAM_STMT is unsupported
    let mut st = Core::prepare(&mut h, &db, "SELECT ?").unwrap();
    st.bind(Ok(0), Value::Int(1), php_sqlite::pdo::PARAM_STMT);
    assert!(matches!(
        st.execute(&mut h, None),
        Err(CoreError::Hook(HookError::Silent))
    ));
    // pdo_sqlite_tostring_exception
    exec(&mut h, &db, "CREATE TABLE t(id int, v varchar(255))").unwrap();
    let mut st = Core::prepare(&mut h, &db, "INSERT INTO t VALUES(:i, :v)").unwrap();
    st.bind(Err("i"), Value::Int(1234), PARAM_STR);
    st.bind(
        Err("v"),
        Value::Reference(PhpRef::new(throws_object())),
        PARAM_STR,
    );
    for _ in 0..2 {
        let Err(CoreError::Throw(e)) = st.execute(&mut h, None) else {
            panic!()
        };
        assert_eq!(err_text(&e), "Exception: Sorry");
    }
    let mut q = query(&mut h, &db, "SELECT * FROM t").unwrap();
    assert!(q.fetch_all(&mut h).unwrap().is_empty());
    // bug78192: schema change between executions
    exec(
        &mut h,
        &db,
        "CREATE TABLE user (id INTEGER PRIMARY KEY NOT NULL, name VARCHAR(255) NOT NULL)",
    )
    .unwrap();
    let mut ins = Core::prepare(
        &mut h,
        &db,
        "INSERT INTO user (id, name) VALUES(:id, :name)",
    )
    .unwrap();
    ins.execute(
        &mut h,
        Some(vec![(Err("id"), Value::Int(10)), (Err("name"), s("test"))]),
    )
    .unwrap();
    let mut sel = Core::prepare(&mut h, &db, "SELECT * FROM user WHERE id = :id").unwrap();
    sel.execute(&mut h, Some(vec![(Err("id"), Value::Int(10))]))
        .unwrap();
    assert_eq!(sel.fetch_all(&mut h).unwrap().len(), 1);
    exec(&mut h, &db, "ALTER TABLE user ADD new_col VARCHAR(255)").unwrap();
    sel.execute(&mut h, Some(vec![(Err("id"), Value::Int(10))]))
        .unwrap();
    assert_eq!(sel.columns, 3);
    assert_eq!(sel.names(), ["id", "name", "new_col"]);
}

#[test]
fn connect_errors_and_open_flags() {
    let mut h = TestHost::new();
    // bug50728
    let e = connect(&mut h, "sqlite:/this/path/should/not/exist.db", None).unwrap_err();
    let ConnectError::Driver { sqlstate, code, .. } = &e else {
        panic!()
    };
    assert_eq!((sqlstate.as_str(), *code), ("HY000", 14));
    assert_eq!(
        e.message(),
        "SQLSTATE[HY000] [14] unable to open database file"
    );
    // pdo_sqlite_empty_filename
    let db = connect(&mut h, "sqlite:", None).unwrap();
    assert_eq!(
        exec(
            &mut h,
            &db,
            "CREATE TABLE test_sqlite_empty_filename (id INT);"
        )
        .unwrap(),
        0
    );
    let db = connect(&mut h, "sqlite:file:?cache=shared", None).unwrap();
    assert_eq!(exec(&mut h, &db, "CREATE TABLE x (id INT);").unwrap(), 0);
    // pdo_sqlite_open_flags / filename_uri: read-only databases
    let path = std::env::temp_dir().join(format!("php-sqlite-pdo-{}.db", std::process::id()));
    let p = path.to_string_lossy().into_owned();
    let db = connect(&mut h, &format!("sqlite:{p}"), None).unwrap();
    assert_eq!(
        exec(&mut h, &db, "CREATE TABLE test_sqlite_open_flags (id INT);").unwrap(),
        0
    );
    let mut opts = php_runtime::HashTable::new();
    opts.insert(
        php_runtime::ArrayKey::Int(PDO_SQLITE_ATTR_OPEN_FLAGS),
        Value::Int(SQLITE3_OPEN_READONLY),
    );
    let ro = connect(
        &mut h,
        &format!("sqlite:{p}"),
        Some(&ZArray::from_table(opts)),
    )
    .unwrap();
    let st = exec(
        &mut h,
        &ro,
        "CREATE TABLE test_sqlite_open_flags_2 (id INT);",
    )
    .unwrap_err();
    assert_eq!(
        pdo_message(&ro, st),
        "SQLSTATE[HY000]: General error: 8 attempt to write a readonly database"
    );
    let ro = connect(&mut h, &format!("sqlite:file:{p}?mode=ro"), None).unwrap();
    let st = exec(&mut h, &ro, "CREATE TABLE x (id INT);").unwrap_err();
    assert_eq!(
        pdo_message(&ro, st),
        "SQLSTATE[HY000]: General error: 8 attempt to write a readonly database"
    );
    std::fs::remove_file(&path).unwrap();
    // open_basedir
    h.open_basedir = Some(b"/nonexistent-basedir".to_vec());
    for dsn in [
        "sqlite:../not_in_open_basedir.sqlite",
        "sqlite:file:../not_in_open_basedir.sqlite?mode=ro",
    ] {
        let ConnectError::Exception(e) = connect(&mut h, dsn, None).unwrap_err() else {
            panic!()
        };
        assert_eq!(
            err_text(&e),
            format!("PDOException: open_basedir prohibits opening {}", &dsn[7..])
        );
    }
    assert!(connect(&mut h, "sqlite::memory:", None).is_ok());
    assert!(PdoSqliteDbh::split_dsn(b"nodsn").is_none());
}

#[test]
fn blob_and_load_extension() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (id STRING, data BLOB)").unwrap();
    let mut st = Core::prepare(&mut h, &db, "INSERT INTO test (id, data) VALUES (?, ?)").unwrap();
    st.bind(Ok(0), s("a"), PARAM_STR);
    st.bind(Ok(1), s("TEST TEST"), PARAM_LOB);
    st.execute(&mut h, None).unwrap();
    let mut cx = Cx::new(&mut h);
    let blob = db
        .open_blob(&mut cx, b"test", b"data", 1, b"main", SQLITE3_OPEN_READONLY)
        .unwrap();
    let mut stream = blob.into_stream();
    assert_eq!(stream.ops_label(), "PDOSQLite");
    assert_eq!(stream.read_vec(100).unwrap(), b"TEST TEST");
    php_streams::diag::take();
    assert!(stream.write(b"ABCD").is_err());
    assert_eq!(
        php_streams::diag::take()[0].text(),
        "Can't write to blob stream: is open as read only"
    );
    let mut stream = db
        .open_blob(
            &mut cx,
            b"test",
            b"data",
            1,
            b"main",
            SQLITE3_OPEN_READWRITE,
        )
        .unwrap()
        .into_stream();
    assert_eq!(stream.write(b"ABCD"), Ok(4));
    stream.seek(0, 0);
    assert_eq!(stream.read_vec(100).unwrap(), b"ABCD TEST");
    assert_eq!(stream.seek(i64::MIN, 2), -1);
    assert!(db
        .open_blob(&mut cx, b"nope", b"data", 1, b"main", SQLITE3_OPEN_READONLY)
        .is_none());
    drop(cx);
    assert_eq!(
        h.take_log(),
        ["Warning: Unable to open blob: no such table: main.nope"]
    );
    // loadExtension failures
    let mut cx = Cx::new(&mut h);
    assert_eq!(
        err_text(&db.load_extension(&mut cx, b"").unwrap_err()),
        "ValueError: Pdo\\Sqlite::loadExtension(): Argument #1 ($name) must not be empty"
    );
    assert_eq!(
        err_text(
            &db.load_extension(&mut cx, b"/this/does/not_exist")
                .unwrap_err()
        ),
        "PDOException: Unable to load extension \"/this/does/not_exist\""
    );
    let e = db.load_extension(&mut cx, file!().as_bytes()).err();
    if let Some(e) = e {
        assert!(err_text(&e).starts_with("PDOException: Unable to load extension \""));
    }
}

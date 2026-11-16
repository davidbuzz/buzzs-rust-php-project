// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Hand ports of the core-level `ext/pdo_sqlite/tests/*.phpt`.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::disallowed_names,
    clippy::doc_markdown
)]

mod common;

use common::*;
use php_pdo::consts::*;
use php_pdo::ParamId;
use php_runtime::{ArrayKey, PhpRef, RuntimeError, Value, ZArray};

fn mem(h: &mut TestHost) -> Value {
    h.new_pdo("PDO", "sqlite::memory:", None).unwrap()
}

fn print_err(h: &mut TestHost, e: &RuntimeError) {
    let d = h.describe_error(e);
    h.echo(&format!("{d}\n"));
}

fn persistent() -> ZArray {
    let mut o = ZArray::new();
    o.make_mut()
        .insert(ArrayKey::Int(ATTR_PERSISTENT), Value::Bool(true));
    o
}

#[test]
fn bug33841_row_count() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test_33841 (text)").unwrap();
    let name = PhpRef::new(Value::Null);
    for (sql, v) in [
        ("INSERT INTO test_33841 VALUES ( :text )", "test1"),
        ("UPDATE test_33841 SET text = :text ", "test2"),
    ] {
        let st = h.prepare(&db, sql).unwrap();
        h.bind_param(&st, ParamId::Name(z(":text")), &name, PARAM_STR)
            .unwrap();
        name.set(s(v));
        let r = h.execute(&st, None).unwrap();
        h.dump(&Value::Bool(r));
        let n = TestHost::stmt(&st).row_count(&mut h).unwrap();
        h.dump(&i(n));
    }
    assert_phpt!(h, "pdo_sqlite", "bug33841.phpt");
}

#[test]
fn bug35336_magic_set() {
    let mut h = TestHost::new();
    h.add_class(Class::new("EEE").setter(|h, _, _, _| {
        h.echo("hello world\n");
        Ok(())
    }));
    let a = mem(&mut h);
    h.query(
        &a,
        "CREATE TABLE test_35336 (a integer primary key, b text)",
    )
    .unwrap();
    let b = h
        .prepare(&a, "insert into test_35336 (b) values (?)")
        .unwrap();
    h.execute(&b, Some(arr(vec![i(5)]))).unwrap();
    let q = h.query(&a, "SELECT * FROM test_35336").unwrap();
    h.fetch_all(&q, Some(FETCH_CLASS), &[s("EEE")]).unwrap();
    h.echo("Done\n");
    assert_phpt!(h, "pdo_sqlite", "bug35336.phpt");
}

#[test]
fn bug38334_native_types() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.exec(
        &db,
        "CREATE TABLE test_38334 (i INTEGER , f DOUBLE, s VARCHAR(255))",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO test_38334 VALUES (42, 46.7, 'test')")
        .unwrap();
    let q = h.query(&db, "SELECT * FROM test_38334").unwrap();
    let r = h.fetch(&q, FETCH_ASSOC).unwrap();
    h.dump(&r);
    h.exec(&db, "INSERT INTO test_38334 VALUES (10000000000, 0.0, '')")
        .unwrap();
    let q = h
        .query(&db, "SELECT i FROM test_38334 WHERE f = 0.0")
        .unwrap();
    let v = h.fetch_column(&q, 0).unwrap();
    h.dump(&Value::Bool(matches!(v, Value::Int(10_000_000_000))));
    h.exec(&db, "INSERT INTO test_38334 VALUES ('test', 'test', 'x')")
        .unwrap();
    let q = h
        .query(&db, "SELECT * FROM test_38334 WHERE s = 'x'")
        .unwrap();
    let r = h.fetch(&q, FETCH_ASSOC).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_sqlite", "bug38334.phpt");
}

#[test]
fn bug_42589_column_meta_table() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.exec(&db, "CREATE TABLE test_42589 (field1 VARCHAR(10))")
        .unwrap();
    h.exec(&db, "INSERT INTO test_42589 VALUES('test')")
        .unwrap();
    let r = h
        .query(
            &db,
            "SELECT * FROM test_42589 t1 LEFT JOIN test_42589 t2 ON t1.field1 = t2.field1",
        )
        .unwrap();
    for c in 0..2 {
        let m = TestHost::stmt(&r).get_column_meta(&mut h, c).unwrap();
        let ok = m
            .as_array()
            .and_then(|a| a.get_str(b"table").cloned())
            .is_some_and(|t| t.as_str().is_some_and(|s| s.as_bytes() == b"test_42589"));
        h.dump(&Value::Bool(ok));
    }
    assert_phpt!(h, "pdo_sqlite", "bug_42589.phpt");
}

#[test]
fn bug43831_persistent_subclasses() {
    let mut h = TestHost::new();
    for c in ["Foo", "Baz", "MyPDO"] {
        h.add_class(Class::new(c).extends("PDO"));
    }
    h.add_class(Class::new("Bar").extends("Baz"));
    let bar = h
        .new_pdo("Bar", "sqlite::memory:", Some(persistent()))
        .unwrap();
    h.echo(&format!(
        "{}\n",
        TestHost::obj(&bar).class_name().to_string_lossy()
    ));
    let foo = h
        .new_pdo("Foo", "sqlite::memory:", Some(persistent()))
        .unwrap();
    assert!(php_pdo::Pdo::same_connection(
        &TestHost::pdo(&bar),
        &TestHost::pdo(&foo)
    ));
    h.echo(&format!(
        "{}\n",
        TestHost::obj(&bar).class_name().to_string_lossy()
    ));
    let bar = h
        .new_pdo("PDO", "sqlite::memory:", Some(persistent()))
        .unwrap();
    let baz = h
        .new_pdo("MyPDO", "sqlite::memory:", Some(persistent()))
        .unwrap();
    h.dump(&bar);
    drop(bar);
    h.dump(&baz);
    h.echo("\nWarning: Undefined variable $bar in /t on line 1\n");
    h.dump(&Value::Null);
    assert_eq!(h.persistent.len(), 1);
    assert_phpt!(h, "pdo_sqlite", "bug43831.phpt");
}

#[test]
fn bug44327_query_string_column() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let x = h.query(&db, "select 1 as queryString").unwrap();
    h.dump(&x);
    let qs = TestHost::get_prop(&x, "queryString").unwrap();
    h.dump(&qs);
    let y = h.fetch(&x, FETCH_DEFAULT).unwrap();
    h.dump(&y);
    h.dump(&Value::Null);
    h.echo("--------------------------------------------\n");
    let x = h.query(&db, "select 1 as queryString").unwrap();
    h.dump(&x);
    h.dump(&qs);
    let y = h.fetch(&x, FETCH_LAZY).unwrap();
    h.dump(&y);
    let row = TestHost::row(&y).unwrap();
    let v = row.read_property(&mut h, &z("queryString")).unwrap();
    h.dump(&v);
    assert_phpt!(h, "pdo_sqlite", "bug44327_2.phpt");

    let mut h = TestHost::new();
    let db = mem(&mut h);
    let x = h.query(&db, "select 1 as queryStringxx").unwrap();
    let y = h.fetch(&x, FETCH_LAZY).unwrap();
    h.dump(&y);
    let row = TestHost::row(&y).unwrap();
    for n in ["queryString", "queryStringzz", "queryStringxx"] {
        let v = row.read_property(&mut h, &z(n)).unwrap();
        h.dump(&v);
    }
    h.echo("---\n");
    let v = row.read_dimension(&mut h, Some(&i(5))).unwrap();
    h.dump(&v);
    let v = row.read_property(&mut h, &z("3")).unwrap();
    h.dump(&v);
    assert_phpt!(h, "pdo_sqlite", "bug44327_3.phpt");
}

#[test]
fn bug46139_props_late() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.add_class(
        Class::new("Person")
            .prop("test_prop")
            .dynamic()
            .ctor(|h, this, _| {
                let v = TestHost::get_prop(this, "test_prop").unwrap();
                h.dump(&v);
                Ok(())
            }),
    );
    let show = |h: &mut TestHost, v: &Value| {
        let p = TestHost::get_prop(v, "test_prop").unwrap();
        h.echo(&format!("'{}'\n", p.as_str().unwrap().to_string_lossy()));
    };
    for n in 0..3 {
        let st = h.query(&db, "SELECT 'foo' test_prop, 1").unwrap();
        h.set_fetch_mode(&st, FETCH_CLASS | FETCH_PROPS_LATE, &[s("Person")])
            .unwrap();
        let r = match n {
            0 => h.fetch(&st, FETCH_DEFAULT).unwrap(),
            1 => {
                let all = h.fetch_all(&st, None, &[]).unwrap();
                all.as_array().unwrap().get_int(0).unwrap().clone()
            }
            _ => h.fetch(&st, FETCH_CLASS | FETCH_PROPS_LATE).unwrap(),
        };
        show(&mut h, &r);
    }
    assert_phpt!(h, "pdo_sqlite", "bug46139.phpt");
}

#[test]
fn bug48773_statement_class_with_pdo_arg() {
    let mut h = TestHost::new();
    h.add_class(
        Class::new("bar")
            .extends("PDOStatement")
            .private_ctor()
            .ctor(|_, _, _| Ok(())),
    );
    h.add_class(Class::new("foo").extends("PDO").prop_default(
        "statementClass",
        Vis::Public,
        s("bar"),
    ));
    let mut o = ZArray::new();
    o.make_mut()
        .insert(ArrayKey::Int(ATTR_ERRMODE), i(ERRMODE_EXCEPTION));
    let db = h.new_pdo("foo", "sqlite::memory:", Some(o)).unwrap();
    h.set_attr(
        &db,
        ATTR_STATEMENT_CLASS,
        Value::Array(arr(vec![s("bar"), Value::Array(arr(vec![db.clone()]))])),
    )
    .unwrap();
    let st = h.query(&db, "SELECT 1").unwrap();
    h.dump(&st);
    assert_phpt!(h, "pdo_sqlite", "bug48773.phpt");
}

#[test]
fn bug50728_and_64705_connect_failure() {
    let mut h = TestHost::new();
    let e = h
        .new_pdo("PDO", "sqlite:/this/path/should/not/exist.db", None)
        .unwrap_err();
    let code = TestHost::error_prop(&e, "code").unwrap();
    h.dump(&code);
    assert_phpt!(h, "pdo_sqlite", "bug50728.phpt");

    let mut h = TestHost::new();
    let e = h
        .new_pdo(
            "PDO",
            "sqlite:./bug64705NonExistingDir/bug64705NonExistingDb",
            None,
        )
        .unwrap_err();
    let info = TestHost::error_prop(&e, "errorInfo").unwrap();
    h.dump(&Value::Bool(
        matches!(&info, Value::Array(a) if !a.is_empty()),
    ));
    assert_phpt!(h, "pdo_sqlite", "bug_64705.phpt");
}

#[test]
fn bug52487_repeated_fetch_into() {
    let mut h = TestHost::new();
    let db = h.factory();
    let st = h.prepare(&db, "select 1 as attr").unwrap();
    for _ in 0..10 {
        let o = h.new_object("stdClass");
        h.set_fetch_mode(&st, FETCH_INTO, &[o]).unwrap();
    }
    h.echo("ok\n");
    assert_phpt!(h, "pdo_sqlite", "bug52487.phpt");
}

#[test]
fn bug_63916_big_int_param() {
    let mut h = TestHost::new();
    let num = 100_004_313_234_244_i64;
    let conn = mem(&mut h);
    h.query(
        &conn,
        "CREATE TABLE users (id INTEGER NOT NULL, num INTEGER NOT NULL, PRIMARY KEY(id))",
    )
    .unwrap();
    let st = h
        .prepare(&conn, "insert into users (id, num) values (:id, :num)")
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":id")), i(1), PARAM_INT)
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":num")), i(num), PARAM_INT)
        .unwrap();
    h.execute(&st, None).unwrap();
    let q = h.query(&conn, "SELECT num FROM users").unwrap();
    let r = h.fetch_all(&q, Some(FETCH_COLUMN), &[]).unwrap();
    h.dump(&i(num));
    let first = r.as_array().unwrap().get_int(0).unwrap().clone();
    h.dump(&first);
    assert_phpt!(h, "pdo_sqlite", "bug_63916.phpt");
}

#[test]
fn bug66033_statement_ctor_throws() {
    let mut h = TestHost::new();
    h.add_class(
        Class::new("DBStatement")
            .extends("PDOStatement")
            .prop("dbh")
            .private_ctor()
            .ctor(|_, _, _| {
                Err(RuntimeError::Custom {
                    class: z("Exception"),
                    message: "Blah".into(),
                })
            }),
    );
    let pdo = mem(&mut h);
    h.set_attr(
        &pdo,
        ATTR_STATEMENT_CLASS,
        Value::Array(arr(vec![
            s("DBStatement"),
            Value::Array(arr(vec![pdo.clone()])),
        ])),
    )
    .unwrap();
    h.exec(&pdo, "CREATE TABLE IF NOT EXISTS messages (id INTEGER PRIMARY KEY, title TEXT, message TEXT, time INTEGER)")
        .unwrap();
    if let Err(e) = h.query(&pdo, "select * from messages") {
        print_err(&mut h, &e);
    }
    assert_phpt!(h, "pdo_sqlite", "bug66033.phpt");
}

#[test]
fn debugdumpparams_001() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let x = h.prepare(&db, "select :a, :b, ?").unwrap();
    h.bind_value(&x, ParamId::Name(z(":a")), i(1), PARAM_INT)
        .unwrap();
    h.bind_value(&x, ParamId::Name(z(":b")), s("foo"), PARAM_STR)
        .unwrap();
    h.bind_value(&x, ParamId::Position(3), i(1313), PARAM_STR)
        .unwrap();
    let r = h.debug_dump(&x).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_sqlite", "debugdumpparams_001.phpt");
}

#[test]
fn silent_impl_error() {
    let mut h = TestHost::new();
    let pdo = mem(&mut h);
    h.set_attr(&pdo, ATTR_ERRMODE, i(ERRMODE_SILENT)).unwrap();
    h.error_handler = Some(std::rc::Rc::new(|h: &mut TestHost, m: &str| {
        h.echo(&format!("warning: {m}\n"));
        Ok(())
    }));
    let r = h.get_attr(&pdo, 123_456).unwrap();
    h.echo("result: ");
    h.dump(&r);
    h.echo("errorInfo: ");
    let info = h.db_error_info(&pdo).unwrap();
    let first = info.as_array().unwrap().get_int(0).unwrap().clone();
    h.dump(&first);
    assert_phpt!(h, "pdo_sqlite", "pdo_silent_impl_error.phpt");
}

#[test]
fn failed_execute() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let st = h
        .prepare(&db, "SELECT ? AS value UNION ALL SELECT 2")
        .unwrap();
    h.execute(&st, Some(arr(vec![s("first")]))).unwrap();
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    let o = h.new_object("stdClass");
    if let Err(e) = h.execute(&st, Some(arr(vec![o]))) {
        print_err(&mut h, &e);
    }
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    h.execute(&st, Some(arr(vec![s("third")]))).unwrap();
    let v = h.fetch_all(&st, Some(FETCH_COLUMN), &[]).unwrap();
    h.dump(&v);
    let out_before_udf = h.output();
    // UDF part: the driver method lives in php-sqlite
    let sq = TestHost::pdo(&db).sqlite().unwrap();
    h.add_function("fail_execute", |_, a| {
        if matches!(a[0].deref(), Value::Int(1))
            || matches!(a[0].deref(), Value::Str(ref s) if s.as_bytes() == b"1")
        {
            return Err(RuntimeError::Custom {
                class: z("RuntimeException"),
                message: "execution failed".into(),
            });
        }
        Ok(s("first"))
    });
    assert!(sq.create_function(b"fail_execute", s("fail_execute"), -1, 0));
    let st = h
        .prepare(&db, "SELECT fail_execute(?) AS value UNION ALL SELECT 2")
        .unwrap();
    h.execute(&st, Some(arr(vec![i(0)]))).unwrap();
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    if let Err(e) = h.execute(&st, Some(arr(vec![i(1)]))) {
        print_err(&mut h, &e);
    }
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    h.execute(&st, Some(arr(vec![i(0)]))).unwrap();
    let v = h.fetch_all(&st, Some(FETCH_COLUMN), &[]).unwrap();
    h.dump(&v);
    h.exec(&db, "CREATE TABLE failed_execute (value INTEGER UNIQUE)")
        .unwrap();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    let st = h
        .prepare(&db, "INSERT INTO failed_execute VALUES (?)")
        .unwrap();
    h.execute(&st, Some(arr(vec![i(1)]))).unwrap();
    let warnings = std::rc::Rc::new(std::cell::Cell::new(0));
    let w = warnings.clone();
    h.error_handler = Some(std::rc::Rc::new(move |_: &mut TestHost, _: &str| {
        w.set(w.get() + 1);
        Ok(())
    }));
    let r = h.execute(&st, Some(arr(vec![i(1)]))).unwrap();
    h.error_handler = None;
    h.dump(&Value::Bool(r));
    h.echo(&format!("driver warnings: {}\n", warnings.get()));
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    // the deprecation of PDO::sqliteCreateFunction() is the VM's business
    let out = h.output();
    let tail = &out[out_before_udf.len()..];
    let full = format!(
        "{out_before_udf}\nDeprecated: Method PDO::sqliteCreateFunction() is deprecated since 8.5, use Pdo\\Sqlite::createFunction() instead in /t on line 1\n{tail}"
    );
    assert!(check_phpt(
        "pdo_sqlite",
        "pdo_sqlite_failed_execute.phpt",
        &full
    ));
}

#[test]
fn last_insert_id_and_transactions() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.query(
        &db,
        "CREATE TABLE test_pdo_sqlite_lastinsertid (id INT AUTO INCREMENT, name TEXT)",
    )
    .unwrap();
    h.query(
        &db,
        "INSERT INTO test_pdo_sqlite_lastinsertid VALUES (NULL, 'PHP'), (NULL, 'PHP6')",
    )
    .unwrap();
    let q = h
        .query(&db, "SELECT * FROM test_pdo_sqlite_lastinsertid")
        .unwrap();
    h.dump(&q);
    let info = h.db_error_info(&db).unwrap();
    h.dump(&info);
    let id = TestHost::pdo(&db).last_insert_id(&mut h, None).unwrap();
    h.dump(&id);
    assert_phpt!(h, "pdo_sqlite", "pdo_sqlite_lastinsertid.phpt");

    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    let pdo = TestHost::pdo(&db);
    pdo.begin_transaction(&mut h).unwrap();
    h.query(
        &db,
        "CREATE TABLE test_pdo_sqlite_transaction (id INT AUTO INCREMENT, name TEXT)",
    )
    .unwrap();
    pdo.commit(&mut h).unwrap();
    pdo.begin_transaction(&mut h).unwrap();
    h.query(
        &db,
        "INSERT INTO test_pdo_sqlite_transaction VALUES (NULL, 'PHP'), (NULL, 'PHP6')",
    )
    .unwrap();
    pdo.roll_back(&mut h).unwrap();
    let r = h
        .query(&db, "SELECT COUNT(*) FROM test_pdo_sqlite_transaction")
        .unwrap();
    let n = TestHost::stmt(&r).row_count(&mut h).unwrap();
    h.dump(&i(n));
    h.query(&db, "DROP TABLE test_pdo_sqlite_transaction")
        .unwrap();
    assert_phpt!(h, "pdo_sqlite", "pdo_sqlite_transaction.phpt");
}

#[test]
fn reentrant_bind() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let make = |h: &mut TestHost, class: &str, st: &Value| {
        let st2 = st.clone();
        let c = class.to_string();
        h.add_class(
            Class::new(class).method("__toString", false, Vis::Public, move |h, _, _| {
                let r = match c.as_str() {
                    "Rebind" => h
                        .bind_value(&st2, ParamId::Position(1), s("x"), PARAM_STR)
                        .map(|_| "bindValue"),
                    "Reexec" => h
                        .execute(&st2, Some(arr(vec![s("x"), s("y")])))
                        .map(|_| "execute"),
                    _ => h.close_cursor(&st2).map(|_| "closeCursor"),
                };
                match r {
                    Ok(what) => h.echo(&format!("{what}: no error\n")),
                    Err(e) => {
                        let d = h.describe_error(&e);
                        h.echo(&format!("{d}\n"));
                    }
                }
                Ok(s(&c.to_ascii_lowercase()))
            }),
        );
        h.new_object(class)
    };
    for (label, class) in [
        ("bindValue", "Rebind"),
        ("execute", "Reexec"),
        ("closeCursor", "Reclose"),
    ] {
        h.echo(&format!("{label}:\n"));
        let st = h.prepare(&db, "SELECT ?, ?").unwrap();
        let p1 = PhpRef::new(s("placeholder"));
        let p2 = PhpRef::new(s("second"));
        h.bind_param(&st, ParamId::Position(1), &p1, PARAM_STR)
            .unwrap();
        h.bind_param(&st, ParamId::Position(2), &p2, PARAM_STR)
            .unwrap();
        let o = make(&mut h, class, &st);
        p1.set(o);
        match h.execute(&st, None) {
            Ok(_) => h.echo(&format!("execute after {label}: no error\n")),
            Err(e) => print_err(&mut h, &e),
        }
    }
    h.echo("done\n");
    assert_phpt!(h, "pdo_sqlite", "pdo_sqlite_reentrant_bind.phpt");
}

#[test]
fn subclasses_connect() {
    for (file, connect) in [("pdosqlite_001.phpt", false), ("pdosqlite_002.phpt", true)] {
        let mut h = TestHost::new();
        let db = if connect {
            h.connect("PDO", "sqlite::memory:", None).unwrap()
        } else {
            h.new_pdo("Pdo\\Sqlite", "sqlite::memory:", None).unwrap()
        };
        assert!(h.is_a(
            &TestHost::obj(&db).class_name().to_string_lossy(),
            "Pdo\\Sqlite"
        ));
        h.query(&db, "CREATE TABLE t (id INT AUTO INCREMENT, name TEXT)")
            .unwrap();
        h.query(&db, "INSERT INTO t VALUES (NULL, 'PHP')").unwrap();
        h.query(&db, "INSERT INTO t VALUES (NULL, 'PHP6')").unwrap();
        h.add_function("lower", |_, a| {
            Ok(Value::Str(a[0].as_str().unwrap().to_ascii_lowercase()))
        });
        let sq = TestHost::pdo(&db).sqlite().unwrap();
        assert!(sq.create_function(b"testing", s("lower"), 1, 2048));
        let q = h.query(&db, "SELECT testing(name) FROM t").unwrap();
        h.foreach(&q, |h, _, row| h.dump(&row)).unwrap();
        h.echo("Fin.");
        assert_phpt!(h, "pdo_sqlite", &format!("subclasses/{file}"));
    }

    let mut h = TestHost::new();
    h.add_class(Class::new("MyPdoSqlite").extends("Pdo\\Sqlite"));
    let db = h.connect("MyPdoSqlite", "sqlite::memory:", None).unwrap();
    if TestHost::obj(&db).class_name().to_string_lossy() == "MyPdoSqlite" {
        h.echo("OK");
    }
    assert_phpt!(h, "pdo_sqlite", "subclasses/pdosqlite_006.phpt");

    let mut h = TestHost::new();
    h.add_class(Class::new("MyPDO").extends("PDO"));
    if let Err(e) = h.connect("MyPDO", "sqlite::memory:", None) {
        print_err(&mut h, &e);
    }
    assert_phpt!(h, "pdo_sqlite", "subclasses/pdosqlite_007.phpt");
}

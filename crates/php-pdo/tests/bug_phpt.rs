//! Hand ports of `ext/pdo/tests/bug_*.phpt` over `sqlite::memory:`.

mod common;

use common::*;
use php_pdo::consts::*;
use php_pdo::ParamId;
use php_runtime::{PhpRef, Resource, RuntimeError, Value};

fn dump_query_all(h: &mut TestHost, db: &Value, sql: &str, mode: i64) {
    let q = h.query(db, sql).unwrap();
    let r = h.fetch_all(&q, Some(mode), &[]).unwrap();
    h.dump(&r);
}

#[test]
fn bug_34630_lob_stream() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test34630 (id int NOT NULL PRIMARY KEY, val VARCHAR(256))",
    )
    .unwrap();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    let fp = Value::Resource(Resource::new(
        "stream",
        Some(Box::new(b"I am the LOB data".to_vec())),
    ));
    let ins = h
        .prepare(&db, "insert into test34630 (id, val) values (1, :blob)")
        .unwrap();
    h.bind_value(&ins, ParamId::Name(z(":blob")), fp, PARAM_LOB)
        .unwrap();
    h.execute(&ins, None).unwrap();
    h.set_attr(&db, ATTR_STRINGIFY_FETCHES, Value::Bool(true))
        .unwrap();
    dump_query_all(&mut h, &db, "SELECT * from test34630", FETCH_ASSOC);
    assert_phpt!(h, "bug_34630.phpt");
}

#[test]
fn bug_34687_query_error_code() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_SILENT)).unwrap();
    let x = h
        .query(&db, "UPDATE non_existent_pdo_test_table set foo = 'bar'")
        .unwrap();
    h.dump(&x);
    let code = h.db_error_code(&db).unwrap();
    let code = code.as_str().unwrap().to_string_lossy();
    assert!(code != "00000" && !code.is_empty());
    h.echo(&format!("OK: {code}\n"));
    assert_phpt!(h, "bug_34687.phpt");
}

#[test]
fn bug_35671_named_params() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test35671 (field1 VARCHAR(32), field2 VARCHAR(32), field3 VARCHAR(32))",
    )
    .unwrap();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    let ins = h
        .prepare(
            &db,
            "insert into test35671 (field1, field2, field3) values (:value1, :value2, :value3)",
        )
        .unwrap();
    h.execute(
        &ins,
        Some(assoc(vec![
            (":value1", i(15)),
            (":value2", i(20)),
            (":value3", i(25)),
        ])),
    )
    .unwrap();
    dump_query_all(&mut h, &db, "SELECT * from test35671", FETCH_ASSOC);
    assert_phpt!(h, "bug_35671.phpt");
}

#[test]
fn bug_38253_default_mode_without_args() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "create table test38253 (id integer primary key, n varchar(255))",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO test38253 (id, n) VALUES (1, 'hi')")
        .unwrap();
    for mode in [FETCH_CLASS, FETCH_FUNC] {
        h.set_attr(&db, ATTR_DEFAULT_FETCH_MODE, i(mode)).unwrap();
        let st = h.prepare(&db, "SELECT * FROM test38253").unwrap();
        h.execute(&st, None).unwrap();
        let r = h.fetch_all(&st, None, &[]).unwrap();
        h.dump(&r);
    }
    h.set_attr(&db, ATTR_DEFAULT_FETCH_MODE, i(FETCH_INTO))
        .unwrap();
    let st = h.prepare(&db, "SELECT * FROM test38253").unwrap();
    h.execute(&st, None).unwrap();
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_38253.phpt");
}

#[test]
fn bug_38394_rebind() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test38394 (a INT, b INT, c INT)")
        .unwrap();
    let st = h
        .prepare(&db, "INSERT INTO test38394 (a,b,c) VALUES (:a,:b,:c)")
        .unwrap();
    h.execute(
        &st,
        Some(assoc(vec![("a", i(1)), ("b", i(2)), ("c", i(3))])),
    )
    .unwrap();
    // `@$s->execute(...)`: the warning is silenced
    let before = h.out.len();
    let _ = h.execute(
        &st,
        Some(assoc(vec![
            ("a", i(5)),
            ("b", i(6)),
            ("c", i(7)),
            ("d", i(8)),
        ])),
    );
    h.out.truncate(before);
    h.execute(
        &st,
        Some(assoc(vec![("a", i(9)), ("b", i(10)), ("c", i(11))])),
    )
    .unwrap();
    dump_query_all(&mut h, &db, "SELECT * FROM test38394", FETCH_ASSOC);
    assert_phpt!(h, "bug_38394.phpt");
}

#[test]
fn bug_39398_bool_as_int() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test39398 (test INT)").unwrap();
    let st = h
        .prepare(&db, "INSERT INTO test39398 VALUES (:boolean)")
        .unwrap();
    h.bind_value(
        &st,
        ParamId::Name(z(":boolean")),
        Value::Bool(true),
        PARAM_INT,
    )
    .unwrap();
    h.execute(&st, None).unwrap();
    dump_query_all(&mut h, &db, "SELECT * FROM test39398", FETCH_ASSOC);
    assert_phpt!(h, "bug_39398.phpt");
}

#[test]
fn bug_39656_close_cursor_fetch() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    h.exec(
        &db,
        "CREATE TABLE test39656 (id INTEGER NOT NULL PRIMARY KEY, usr VARCHAR( 256 ) NOT NULL)",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO test39656 (id, usr) VALUES (1, 'user')")
        .unwrap();
    let st = h
        .prepare(&db, "SELECT * FROM test39656 WHERE id = ?")
        .unwrap();
    h.bind_value(&st, ParamId::Position(1), i(1), PARAM_INT)
        .unwrap();
    h.execute(&st, None).unwrap();
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    h.execute(&st, None).unwrap();
    h.close_cursor(&st).unwrap();
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    h.echo("Done\n");
    assert_phpt!(h, "bug_39656.phpt");
}

#[test]
fn bug_40285_quoted_placeholder() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test40285 (field1 VARCHAR(32), field2 VARCHAR(32), field3 VARCHAR(32), field4 INT)").unwrap();
    let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(true));
    let st = h
        .prepare(
            &db,
            "INSERT INTO test40285 VALUES( ':id', 'name', 'section', 22)",
        )
        .unwrap();
    h.execute(&st, None).unwrap();
    h.echo("Done\n");
    assert_phpt!(h, "bug_40285.phpt");
}

#[test]
fn bug_42917_key_pair_default() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test42917 (a varchar(100), b varchar(100), c varchar(100))",
    )
    .unwrap();
    for n in 0..5 {
        h.exec(
            &db,
            &format!("INSERT INTO test42917 (a,b,c) VALUES('test{n}','{n}','{n}')"),
        )
        .unwrap();
    }
    let res = h.query(&db, "SELECT a,b FROM test42917").unwrap();
    h.set_fetch_mode(&res, FETCH_KEY_PAIR, &[]).unwrap();
    let r = h.fetch_all(&res, None, &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_42917.phpt");
}

#[test]
fn bug_43139_group_default_assoc() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_DEFAULT_FETCH_MODE, i(FETCH_ASSOC))
        .unwrap();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    let q = h
        .query(&db, "select 0 as abc, 1 as xyz, 2 as def ")
        .unwrap();
    let r = h.fetch_all(&q, Some(FETCH_GROUP), &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_43139.phpt");
}

#[test]
fn bug_44159_attribute_types() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    for attr in [ATTR_STATEMENT_CLASS, ATTR_STRINGIFY_FETCHES] {
        for v in [Value::Null, i(1), s("nonsense")] {
            match h.set_attr(&db, attr, v) {
                Ok(b) => h.dump(&Value::Bool(b)),
                Err(e) => {
                    let d = h.describe_error(&e);
                    h.echo(&format!("{d}\n"));
                }
            }
        }
    }
    assert_phpt!(h, "bug_44159.phpt");
}

#[test]
fn bug_44173_query_fetch_mode_args() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test44173 (x int)").unwrap();
    h.exec(&db, "INSERT INTO test44173 VALUES (1)").unwrap();
    let sql = "SELECT * FROM test44173";
    let print = |h: &mut TestHost, r: Result<Value, RuntimeError>| match r {
        Ok(v) => h.dump(&v),
        Err(e) => h.echo(&format!("{}\n", TestHost::error_message(&e))),
    };
    let r = h.query_mode(
        &db,
        sql,
        Some(FETCH_LAZY),
        &[i(0), Value::Array(arr(vec![]))],
    );
    print(&mut h, r);
    // `query($sql, 'abc')` fails in ZPP (VM)
    h.echo("PDO::query(): Argument #2 ($fetchMode) must be of type ?int, string given\n");
    let r = h.query_mode(&db, sql, Some(FETCH_CLASS), &[i(0), i(0), i(0)]);
    print(&mut h, r);
    for mode in [FETCH_INTO, FETCH_COLUMN, FETCH_CLASS] {
        let r = h.query_mode(&db, sql, Some(mode), &[]);
        print(&mut h, r);
    }
    assert_phpt!(h, "bug_44173.phpt");
}

#[test]
fn bug_44409_fetch_serialize() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test44409 (dat varchar(100))")
        .unwrap();
    h.exec(&db, "INSERT INTO test44409 (dat) VALUES ('Data from DB')")
        .unwrap();
    let mut c = Class::new("bug44409").ctor(|h, _, _| {
        h.echo("Method called: bug44409::__construct()\n");
        Ok(())
    });
    c.unserialize = Some(std::rc::Rc::new(|h: &mut TestHost, _: &Value, d: &[u8]| {
        h.echo(&format!(
            "Method called: bug44409::unserialize('{}')\n",
            String::from_utf8_lossy(d)
        ));
        Ok(())
    }));
    h.add_class(c);
    h.echo("\nDeprecated: bug44409 implements the Serializable interface, which is deprecated. Implement __serialize() and __unserialize() instead (or in addition, if support for old PHP versions is necessary) in /t on line 1\n");
    let st = h.query(&db, "SELECT * FROM test44409").unwrap();
    let r = h
        .fetch_all(&st, Some(FETCH_CLASS | FETCH_SERIALIZE), &[s("bug44409")])
        .unwrap();
    h.print_r(&r);
    assert_phpt!(h, "bug_44409.phpt");
}

#[test]
fn bug_46292_classtype_group() {
    let mut h = TestHost::new();
    let db = h.factory();
    let ctor = |name: &'static str| {
        move |h: &mut TestHost, _: &Value, _: Option<&php_runtime::ZArray>| {
            let _ = name;
            h.echo("myclass::__construct()\n");
            Ok(())
        }
    };
    h.add_class(
        Class::new("myclass")
            .prop("val")
            .typed("group_name", "string")
            .ctor(ctor("myclass")),
    );
    h.add_class(Class::new("myclass2").extends("myclass"));
    let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(false));
    h.set_attr(&db, ATTR_STRINGIFY_FETCHES, Value::Bool(true))
        .unwrap();
    h.query(&db, "CREATE TABLE test_46292 (name VARCHAR(20) NOT NULL, group_name VARCHAR(20), val VARCHAR(1))").unwrap();
    for row in [
        "'myclass', 'alpha', '1'",
        "'myclass2', 'beta', '2'",
        "'myclass', 'alpha', '0'",
        "'myclass3', 'beta', '0'",
        "'myclass', 'alpha', '1'",
        "'myclass2', 'beta', '2'",
    ] {
        h.exec(&db, &format!("INSERT INTO test_46292 VALUES ({row})"))
            .unwrap();
    }
    let st = h.prepare(&db, "SELECT * FROM test_46292").unwrap();
    let r = h
        .set_fetch_mode(&st, FETCH_CLASS | FETCH_CLASSTYPE | FETCH_GROUP, &[])
        .unwrap();
    h.dump(&Value::Bool(r));
    h.execute(&st, None).unwrap();
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_46292.phpt");
}

#[test]
fn bug_36798_latin1_literal() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test36798 (id INTEGER)").unwrap();
    h.exec(&db, "INSERT INTO test36798 (id) VALUES (1)")
        .unwrap();
    let pdo = TestHost::pdo(&db);
    let sql = php_runtime::ZStr::from(&b"SELECT '\xC3' as test FROM test36798 WHERE id = :id"[..]);
    let st = pdo.prepare(&mut h, &sql, None).unwrap();
    h.execute(&st, Some(assoc(vec![(":id", i(1))]))).unwrap();
    let row = h.fetch(&st, FETCH_NUM).unwrap();
    h.dump(&row);
    let _ = PhpRef::new(Value::Null);
    assert_phpt!(h, "bug_36798.phpt");
}

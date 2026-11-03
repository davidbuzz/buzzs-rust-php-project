//! Hand ports of `ext/pdo/tests/bug_*.phpt` over `sqlite::memory:`.

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

#[test]
fn bug_50458_fetch_func_closure() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test50458 (a VARCHAR(10))")
        .unwrap();
    h.exec(&db, "INSERT INTO test50458 (a) VALUES ('xyz')")
        .unwrap();
    h.add_function("upper", |_, args| {
        let s = args[0].as_str().unwrap().to_ascii_uppercase();
        Ok(Value::Str(s))
    });
    let res = h.query(&db, "SELECT a FROM test50458").unwrap();
    let r = h.fetch_all(&res, Some(FETCH_FUNC), &[s("upper")]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_50458.phpt");
}

#[test]
fn bug_60665_lazy_isset_empty() {
    let mut h = TestHost::new();
    let db = h.factory();
    let st = h
        .prepare(&db, "SELECT NULL AS null_value, 0 AS zero, 1 AS one ")
        .unwrap();
    h.execute(&st, None).unwrap();
    let row = h.fetch(&st, FETCH_LAZY).unwrap();
    let row = TestHost::row(&row).unwrap();
    let empty = |h: &mut TestHost, n: &str| !row.has_property(h, &z(n), true).unwrap();
    let results = [
        empty(&mut h, "null_value"),
        empty(&mut h, "zero"),
        !empty(&mut h, "one"),
        empty(&mut h, "missing"),
    ];
    let isset = |h: &mut TestHost, n: &str| row.has_property(h, &z(n), false).unwrap();
    let results2 = [
        !isset(&mut h, "null_value"),
        isset(&mut h, "zero"),
        isset(&mut h, "one"),
        !isset(&mut h, "missing"),
    ];
    for b in results.into_iter().chain(results2) {
        h.dump(&Value::Bool(b));
    }
    assert_phpt!(h, "bug_60665.phpt");
}

#[test]
fn bug_61292_persistent_subclass() {
    let mut h = TestHost::new();
    h.add_class(Class::new("Database_SQL").extends("PDO").prop_default(
        "bar",
        Vis::Public,
        Value::Array(arr(vec![])),
    ));
    let mut opts = php_runtime::ZArray::new();
    opts.make_mut().insert(
        php_runtime::ArrayKey::Int(ATTR_PERSISTENT),
        Value::Bool(true),
    );
    let db = h
        .new_pdo("Database_SQL", "sqlite::memory:", Some(opts))
        .unwrap();
    let bar = TestHost::get_prop(&db, "bar").unwrap();
    h.dump(&bar);
    assert_phpt!(h, "bug_61292.phpt");
}

#[test]
fn bug_64172_error_info_after_failure() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test64172 (x int)").unwrap();
    h.exec(&db, "INSERT INTO test64172 VALUES (1)").unwrap();
    let check = |h: &mut TestHost| {
        h.echo("===TEST===\n");
        let info = h.db_error_info(&db).unwrap();
        let info = info.as_array().unwrap().clone();
        h.dump(&Value::Bool(matches!(info.get_int(0), Some(Value::Str(_)))));
        h.dump(&Value::Bool(matches!(info.get_int(1), Some(Value::Int(_)))));
        h.dump(&Value::Bool(matches!(info.get_int(2), Some(Value::Str(_)))));
        h.echo("===GOOD===\n");
    };
    h.echo("===FAIL===\n");
    h.query(&db, "SELECT * FROM bad_table").unwrap();
    h.echo("\n");
    check(&mut h);
    let st = h.query(&db, "SELECT * FROM test64172").unwrap();
    h.fetch_all(&st, None, &[]).unwrap();
    drop(st);
    let info = h.db_error_info(&db).unwrap();
    h.dump(&info);
    h.echo("===FAIL===\n");
    h.exec(&db, "INSERT INTO bad_table VALUES(1)").unwrap();
    h.echo("\n");
    check(&mut h);
    h.exec(&db, "INSERT INTO test64172 VALUES (2)").unwrap();
    let info = h.db_error_info(&db).unwrap();
    h.dump(&info);
    assert_phpt!(h, "bug_64172.phpt");
}

#[test]
fn bug_65946_limit_param() {
    let mut h = TestHost::new();
    let db = h.factory();
    let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(true));
    h.exec(&db, "CREATE TABLE test65946(id int)").unwrap();
    h.exec(&db, "INSERT INTO test65946 VALUES(1)").unwrap();
    let st = h
        .prepare(&db, "SELECT * FROM test65946 LIMIT :limit")
        .unwrap();
    h.bind_value(&st, ParamId::Name(z("limit")), i(1), PARAM_INT)
        .unwrap();
    for _ in 0..2 {
        if !h.execute(&st, None).unwrap() {
            let e = h.stmt_error_info(&st).unwrap();
            h.dump(&e);
        }
    }
    let r = h.fetch_all(&st, Some(FETCH_ASSOC), &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_65946.phpt");
}

#[test]
fn bug_69356_debug_dump_long_query() {
    let mut h = TestHost::new();
    let db = h.factory();
    let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(true));
    let q = "SELECT '
    Dumps the information contained by a prepared statement directly on the output. It will provide the SQL query in use, the number of parameters used (Params), the list of parameters, with their name, type (paramtype) as an integer, their key name or position, and the position in the query (if this is supported by the PDO driver, otherwise, it will be -1).
    This is a debug function, which dump directly the data on the normal output.
    Tip:
    As with anything that outputs its result directly to the browser, the output-control functions can be used to capture the output of this function, and save it in a string (for example).
    This will only dumps the parameters in the statement at the moment of the dump. Extra parameters are not stored in the statement, and not displayed.
'";
    let st = h.query(&db, q).unwrap();
    let r = h.debug_dump(&st).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_69356.phpt");
}

#[test]
fn bug_71447_comments() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    h.set_attr(&db, ATTR_DEFAULT_FETCH_MODE, i(FETCH_NUM))
        .unwrap();
    let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(false));
    h.exec(&db, "CREATE TABLE test71447(id int)").unwrap();
    h.exec(&db, "INSERT INTO test71447 VALUES(1)").unwrap();
    let queries = [
        "\n    SELECT -- That's all folks!\n        '\"abc\":8000'\n    FROM test71447\n",
        "\n    SELECT\n        '\"abc\":8001 -- Wat?'\n    FROM test71447\n",
        "\n    SELECT -- That's all folks!\n        '\"abc\":8002'\n    FROM test71447\n",
        "\n    SELECT /* That's all folks! */\n        '\"abc\":8003'\n    FROM test71447\n",
        "\n    SELECT -- Is it only \"single quotes?\n        '\"abc\":8004'\n    FROM test71447\n",
    ];
    for q in queries {
        let st = h.prepare(&db, q).unwrap();
        h.execute(&st, None).unwrap();
        let v = h.fetch_column(&st, 0).unwrap();
        h.dump(&v);
    }
    let st = h
        .prepare(
            &db,
            "\n    SELECT -- What about question marks here?\n        *\n    FROM test71447\n    WHERE id = ?\n",
        )
        .unwrap();
    h.execute(&st, Some(arr(vec![i(1)]))).unwrap();
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    let st = h
        .prepare(
            &db,
            "\n    SELECT -- What about placeholders :bar\n        *\n    FROM test71447\n    WHERE id = :id\n",
        )
        .unwrap();
    h.execute(&st, Some(assoc(vec![("id", i(1))]))).unwrap();
    let v = h.fetch_column(&st, 0).unwrap();
    h.dump(&v);
    assert_phpt!(h, "bug_71447.phpt");
}

#[test]
fn bug_71885_double_question_mark() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    h.exec(&db, "CREATE TABLE test71885 (a int)").unwrap();
    let sql = "SELECT * FROM test71885 WHERE a ?? 1";
    let e = h.exec(&db, sql).unwrap_err();
    h.dump(&Value::Bool(TestHost::error_message(&e).contains('?')));
    for _ in 0..2 {
        let e = h.prepare(&db, sql).unwrap_err();
        h.dump(&Value::Bool(TestHost::error_message(&e).contains('?')));
    }
    assert_phpt!(h, "bug_71885.phpt");
}

#[test]
fn bug_72368_extra_param() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    for emulate in [false, true] {
        let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(emulate));
        let st = h.prepare(&db, "SELECT 1").unwrap();
        match h.execute(&st, Some(assoc(vec![(":bar", i(1))]))) {
            Ok(b) => h.dump(&Value::Bool(b)),
            Err(_) => h.dump(&s("ERR")),
        }
    }
    h.echo("===DONE===\n");
    assert_phpt!(h, "bug_72368.phpt");
}

#[test]
fn bug_72788_persistent_failed_query() {
    let mut h = TestHost::new();
    for _ in 0..2 {
        let mut opts = php_runtime::ZArray::new();
        opts.make_mut().insert(
            php_runtime::ArrayKey::Int(ATTR_PERSISTENT),
            Value::Bool(true),
        );
        let db = h.factory_class("PDO", Some(opts));
        let before = h.out.len();
        let st = h.query(&db, "SELECT 1 FROM TABLE_DOES_NOT_EXIST").unwrap();
        h.out.truncate(before);
        if matches!(st, Value::Bool(false)) {
            h.echo("Statement failed as expected\n");
        }
    }
    h.echo("Done");
    assert_eq!(h.persistent.len(), 1);
    assert_phpt!(h, "bug_72788.phpt");
}

#[test]
fn bug_73234_bind_null_type() {
    let mut h = TestHost::new();
    let db = h.factory();
    let _ = h.set_attr(&db, ATTR_EMULATE_PREPARES, Value::Bool(true));
    h.exec(&db, "CREATE TABLE test73234(id INT)").unwrap();
    let st = h
        .prepare(&db, "INSERT INTO test73234 VALUES(:value)")
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":value")), i(0), PARAM_NULL)
        .unwrap();
    h.execute(&st, None).unwrap();
    h.bind_value(&st, ParamId::Name(z(":value")), Value::Null, PARAM_NULL)
        .unwrap();
    h.execute(&st, None).unwrap();
    dump_query_all(&mut h, &db, "SELECT * FROM test73234", FETCH_ASSOC);
    assert_phpt!(h, "bug_73234.phpt");
}

#[test]
fn bug_79106_numeric_column_names() {
    let mut h = TestHost::new();
    let db = h.factory();
    let st = h
        .query(&db, "SELECT 0 as \"2007\", 0 as \"2008\", 0 as \"2020\"")
        .unwrap();
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_79106.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    let st = h
        .query(&db, "SELECT 11111 as \"1\", 22222 as \"2\"")
        .unwrap();
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "bug_79106_collision.phpt");
}

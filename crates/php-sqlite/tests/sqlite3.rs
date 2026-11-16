// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of the `ext/sqlite3/tests/*.phpt` expectations that can be checked
//! at this layer (the script logic is re-expressed in Rust; values are
//! compared through `var_dump()` output).

#![allow(clippy::format_push_string)]

mod common;

use common::{dump, mask_ints, print_r, s, throws_object, TestHost};
use php_runtime::{PhpRef, Resource, RuntimeError, Value, ZStr};
use php_sqlite::constants::*;
use php_sqlite::{ParamKey, PhpError, Sqlite3, Sqlite3Result};
use php_streams::stream::Stream;

const RWC: i64 = SQLITE3_OPEN_READWRITE | SQLITE3_OPEN_CREATE;

fn mem(h: &mut TestHost) -> Sqlite3 {
    Sqlite3::construct(h, b":memory:", RWC, b"").expect("open")
}

fn exec(h: &mut TestHost, db: &Sqlite3, sql: &str) {
    assert!(
        db.exec(h, sql.as_bytes()).expect("exec"),
        "exec failed: {sql}: {:?}",
        h.log
    );
}

fn err_text(e: &PhpError) -> String {
    format!(
        "{}: {}",
        e.class_name().unwrap_or_default(),
        e.message().unwrap_or_default()
    )
}

/// All remaining rows dumped.
fn rows(h: &mut TestHost, res: &Sqlite3Result, mode: i64) -> String {
    let mut out = String::new();
    loop {
        let row = res.fetch_array(h, mode).expect("fetch");
        if matches!(row, Value::Bool(false)) {
            return out;
        }
        out.push_str(&dump(&row));
    }
}

fn two_rows(h: &mut TestHost, db: &Sqlite3) {
    exec(h, db, "CREATE TABLE test (time INTEGER, id STRING)");
    exec(h, db, "INSERT INTO test (time, id) VALUES (1234, 'a')");
    exec(h, db, "INSERT INTO test (time, id) VALUES (1234, 'b')");
}

const ROW_A: &str = "array(2) {\n  [0]=>\n  int(%d)\n  [1]=>\n  string(1) \"a\"\n}\n";
const ROW_B: &str = "array(2) {\n  [0]=>\n  int(%d)\n  [1]=>\n  string(1) \"b\"\n}\n";

#[test]
fn sqlite3_02_create_and_03_insert() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.at("SQLite3::exec");
    exec(&mut h, &db, "CREATE TABLE test (time INTEGER, id STRING)");
    assert!(!db
        .exec(&mut h, b"CREATE TABLE test (time INTEGER, id STRING)")
        .unwrap());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::exec(): table test already exists"]
    );
    exec(&mut h, &db, "DROP TABLE test");
    two_rows(&mut h, &db);
    let res = db
        .query(&mut h, b"SELECT * FROM test ORDER BY id ASC", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        mask_ints(&rows(&mut h, &res, SQLITE3_NUM)),
        format!("{ROW_A}{ROW_B}")
    );
    assert!(res.finalize(&mut h).unwrap());
    // sqlite3_04/05: update and delete
    exec(&mut h, &db, "UPDATE test SET id = 'c' WHERE id = 'a'");
    exec(&mut h, &db, "DELETE FROM test WHERE id = 'c'");
    let res = db
        .query(&mut h, b"SELECT * FROM test ORDER BY id ASC", true)
        .unwrap()
        .unwrap();
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_B);
    assert!(db.close(&mut h).unwrap());
    assert!(h.log.is_empty());
}

#[test]
fn sqlite3_06_07_prepared_bind_param_and_value() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    two_rows(&mut h, &db);
    let stmt = db
        .prepare(&mut h, b"SELECT * FROM test WHERE id = ? ORDER BY id ASC")
        .unwrap()
        .unwrap();
    let foo = PhpRef::new(s("c"));
    assert!(stmt
        .bind_param(
            ParamKey::Number(1),
            Value::Reference(foo.clone()),
            Some(SQLITE3_TEXT)
        )
        .unwrap());
    foo.set(s("a"));
    let res = stmt.execute(&mut h).unwrap().unwrap();
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_A);
    // sqlite3_26: statement reset restarts the result
    assert!(stmt.reset(&mut h).unwrap());
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_A);
    // sqlite3_27: result reset
    assert!(res.reset(&mut h).unwrap());
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_A);
    // sqlite3_28: clear bindings and rebind
    stmt.reset(&mut h).unwrap();
    assert!(stmt.clear(&mut h).unwrap());
    assert!(stmt
        .bind_value(ParamKey::Number(1), &s("b"), Some(SQLITE3_TEXT))
        .unwrap());
    let res = stmt.execute(&mut h).unwrap().unwrap();
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_B);
    res.finalize(&mut h).unwrap();
    assert!(db.close(&mut h).unwrap());
}

#[test]
fn sqlite3_10_bound_value_name_and_at_name() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    two_rows(&mut h, &db);
    let stmt = db
        .prepare(&mut h, b"SELECT * FROM test WHERE id = :id ORDER BY id ASC")
        .unwrap()
        .unwrap();
    assert!(stmt
        .bind_value(ParamKey::Name(b":id"), &s("a"), Some(SQLITE3_TEXT))
        .unwrap());
    assert!(stmt
        .bind_value(ParamKey::Name(b"id"), &s("a"), Some(SQLITE3_TEXT))
        .unwrap());
    let res = stmt.execute(&mut h).unwrap().unwrap();
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_A);
    let stmt = db
        .prepare(&mut h, b"SELECT * FROM test WHERE id = @id ORDER BY id ASC")
        .unwrap()
        .unwrap();
    assert!(stmt
        .bind_value(ParamKey::Name(b"@id"), &s("a"), Some(SQLITE3_TEXT))
        .unwrap());
    let res = stmt.execute(&mut h).unwrap().unwrap();
    assert_eq!(mask_ints(&rows(&mut h, &res, SQLITE3_NUM)), ROW_A);
    // unknown names / numbers < 1 return false (bug72571)
    assert!(!stmt
        .bind_value(ParamKey::Name(b"nope"), &s("a"), None)
        .unwrap());
    let stmt = db.prepare(&mut h, b"select 1 = ?").unwrap().unwrap();
    assert!(!stmt
        .bind_param(
            ParamKey::Number(0),
            Value::Reference(PhpRef::new(Value::Int(0))),
            None
        )
        .unwrap());
    assert!(stmt.execute(&mut h).unwrap().is_some());
}

#[test]
fn sqlite3_08_udf_and_29_createfunction() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    two_rows(&mut h, &db);
    let up = h.func("strtoupper", |_, a| {
        Ok(Value::Str(a[0].as_str().unwrap().to_ascii_uppercase()))
    });
    let low = h.func("strtolower", |_, a| {
        Ok(Value::Str(a[0].as_str().unwrap().to_ascii_lowercase()))
    });
    assert!(db
        .create_function(b"strtoupper", up.clone(), -1, 0)
        .unwrap());
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT strtoupper('test')", false, true)
                .unwrap()
        ),
        "string(4) \"TEST\"\n"
    );
    assert!(db
        .create_function(b"strtolower", low.clone(), -1, 0)
        .unwrap());
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT strtolower('TEST')", false, true)
                .unwrap()
        ),
        "string(4) \"test\"\n"
    );
    // re-registering replaces the callback
    assert!(db.create_function(b"strtoupper", low, -1, 0).unwrap());
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT strtoupper('tEst')", false, true)
                .unwrap()
        ),
        "string(4) \"test\"\n"
    );
    // sqlite3_37: deterministic flag
    assert!(db
        .create_function(b"up2", up, 1, SQLITE3_DETERMINISTIC)
        .unwrap());
    let res = db
        .query(&mut h, b"SELECT up2(id) FROM test ORDER BY id ASC", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        rows(&mut h, &res, SQLITE3_NUM),
        "array(1) {\n  [0]=>\n  string(1) \"A\"\n}\narray(1) {\n  [0]=>\n  string(1) \"B\"\n}\n"
    );
    // empty name
    let f = h.func("x", |_, _| Ok(Value::Null));
    assert!(!db.create_function(b"", f, -1, 0).unwrap());
}

#[test]
fn udf_argument_and_return_marshalling() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let f = h.func("types", |_, a| {
        Ok(Value::from(
            a.iter().map(Value::type_name).collect::<Vec<_>>().join(","),
        ))
    });
    db.create_function(b"types", f, -1, 0).unwrap();
    let v = db
        .query_single(
            &mut h,
            b"SELECT types(1, 1.5, NULL, 'x', x'00ff')",
            false,
            true,
        )
        .unwrap();
    assert_eq!(dump(&v), "string(28) \"int,float,null,string,string\"\n");
    let f = h.func("ret", |_, a| {
        Ok(match a[0].as_int().unwrap() {
            0 => Value::Int(7),
            1 => Value::Float(2.5),
            2 => Value::Null,
            3 => Value::Bool(true),
            4 => Value::Bool(false),
            _ => Value::from("s"),
        })
    });
    db.create_function(b"ret", f, 1, 0).unwrap();
    let v = db
        .query_single(&mut h, b"SELECT typeof(ret(0)), typeof(ret(1)), typeof(ret(2)), ret(3), ret(4), typeof(ret(4))", true, true)
        .unwrap();
    assert_eq!(
        print_r(&v),
        "Array\n(\n    [typeof(ret(0))] => integer\n    [typeof(ret(1))] => real\n    [typeof(ret(2))] => null\n    [ret(3)] => 1\n    [ret(4)] => \n    [typeof(ret(4))] => text\n)\n"
    );
    // gh25021: 64-bit integers round-trip
    let pass = h.func("pass", |_, a| Ok(a[0].clone()));
    db.create_function(b"my_udf_pass_int", pass, -1, 0).unwrap();
    for v in [10, i64::MIN, i64::MAX] {
        let stmt = db
            .prepare(&mut h, b"select my_udf_pass_int(:v)")
            .unwrap()
            .unwrap();
        stmt.bind_value(ParamKey::Name(b":v"), &Value::Int(v), Some(SQLITE3_INTEGER))
            .unwrap();
        let row = stmt
            .execute(&mut h)
            .unwrap()
            .unwrap()
            .fetch_array(&mut h, SQLITE3_NUM)
            .unwrap();
        assert_eq!(
            row.as_array().unwrap().get_int(0).and_then(Value::as_int),
            Some(v)
        );
    }
}

#[test]
fn sqlite3_25_create_aggregate() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (a INTEGER, b INTEGER)");
    for (a, b) in [(1, -1), (2, -2), (3, -3), (4, -4), (4, -4)] {
        exec(
            &mut h,
            &db,
            &format!("INSERT INTO test (a, b) VALUES ({a}, {b})"),
        );
    }
    // The context is a comma separated running-total list.
    let step = h.func("sum_list_step", |h, a| {
        h.log.push(format!("step rows={}", a[1].as_int().unwrap()));
        let ctx = a[0].as_str().map(ZStr::to_string_lossy).unwrap_or_default();
        let total: i64 = ctx
            .rsplit(',')
            .next()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        let total = total + a[2].as_int().unwrap();
        Ok(Value::from(if ctx.is_empty() {
            total.to_string()
        } else {
            format!("{ctx},{total}")
        }))
    });
    let fini = h.func("sum_list_finalize", |h, a| {
        h.log.push(format!("final rows={}", a[1].as_int().unwrap()));
        Ok(a[0].clone())
    });
    assert!(db.create_aggregate(b"S", step, fini, 1).unwrap());
    let v = db
        .query_single(&mut h, b"SELECT S(a), S(b) FROM test", true, true)
        .unwrap();
    assert_eq!(
        print_r(&v),
        "Array\n(\n    [S(a)] => 1,3,6,10,14\n    [S(b)] => -1,-3,-6,-10,-14\n)\n"
    );
    let log = h.take_log();
    assert_eq!(log.len(), 12);
    assert_eq!(log[0], "step rows=1");
    assert_eq!(log[9], "step rows=5");
    // xFinal receives the row counter reset to 0
    assert_eq!(log[10], "final rows=0");
    // empty group: final with a NULL context
    exec(&mut h, &db, "DELETE FROM test");
    let v = db
        .query_single(&mut h, b"SELECT S(a) FROM test", false, true)
        .unwrap();
    assert_eq!(dump(&v), "NULL\n");
    assert_eq!(h.take_log(), ["final rows=0"]);
}

#[test]
fn sqlite3_36_create_collation() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let nat = h.func("strnatcmp", |h, a| {
        h.log.push("cmp".into());
        let key = |v: &Value| -> (String, i64) {
            let s = v.as_str().unwrap().to_string_lossy();
            let (p, n) = s.split_at(1);
            (p.to_string(), n.parse().unwrap_or(0))
        };
        Ok(Value::Int(match key(&a[0]).cmp(&key(&a[1])) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }))
    });
    assert!(db.create_collation(b"NAT", nat.clone()).unwrap());
    assert!(!db.create_collation(b"", nat).unwrap());
    exec(&mut h, &db, "CREATE TABLE t (s varchar(4))");
    let stmt = db
        .prepare(&mut h, b"INSERT INTO t VALUES (?)")
        .unwrap()
        .unwrap();
    for v in ["a1", "a10", "a2"] {
        stmt.bind_param(
            ParamKey::Number(1),
            Value::Reference(PhpRef::new(s(v))),
            None,
        )
        .unwrap();
        stmt.execute(&mut h).unwrap().unwrap();
    }
    let default = db
        .query(&mut h, b"SELECT s FROM t ORDER BY s", true)
        .unwrap()
        .unwrap();
    let natural = db
        .query(&mut h, b"SELECT s FROM t ORDER BY s COLLATE NAT", true)
        .unwrap()
        .unwrap();
    let col = |h: &mut TestHost, r: &Sqlite3Result| {
        let mut out = vec![];
        loop {
            let row = r.fetch_array(h, SQLITE3_BOTH).unwrap();
            let Some(a) = row.as_array() else { break };
            out.push(a.get_str(b"s").unwrap().as_str().unwrap().to_string_lossy());
        }
        out
    };
    assert_eq!(col(&mut h, &default), ["a1", "a10", "a2"]);
    assert_eq!(col(&mut h, &natural), ["a1", "a2", "a10"]);
    assert!(h.take_log().iter().all(|l| l == "cmp"));
    // invalid return type
    let bad = h.func("bad", |_, _| Ok(Value::from("x")));
    db.create_collation(b"BAD", bad).unwrap();
    h.at("SQLite3::query");
    let r = db
        .query(&mut h, b"SELECT s FROM t ORDER BY s COLLATE BAD", true)
        .unwrap()
        .unwrap();
    let _ = rows(&mut h, &r, SQLITE3_NUM);
    assert!(h.take_log().contains(
        &"Warning: SQLite3::query(): An error occurred while invoking the compare callback (invalid return type).  Collation behaviour is undefined."
            .to_string()
    ));
}

#[test]
fn bug68760_collation_exception() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let oops = h.func("oopsFunction", |h, _| {
        h.log.push("callback".into());
        Err(RuntimeError::Custom {
            class: ZStr::from("Exception"),
            message: "oops".into(),
        })
    });
    exec(&mut h, &db, "CREATE TABLE test2 (col1 string)");
    exec(
        &mut h,
        &db,
        "INSERT INTO test2 VALUES ('a1'), ('a10'), ('a2')",
    );
    db.create_collation(b"NATURAL_CMP", oops).unwrap();
    let r = db.query(
        &mut h,
        b"SELECT col1 FROM test2 ORDER BY col1 COLLATE NATURAL_CMP",
        true,
    );
    let e = r.expect_err("exception");
    assert_eq!(err_text(&e), "Exception: oops");
    assert_eq!(h.take_log(), ["callback"]);
}

#[test]
fn bug72668_udf_exception() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let f = h.func("my_udf_md5", |_, _| {
        Err(RuntimeError::Custom {
            class: ZStr::from("Exception"),
            message: "test exception\n".into(),
        })
    });
    db.create_function(b"my_udf_md5", f, -1, 0).unwrap();
    let e = db
        .query(&mut h, b"SELECT my_udf_md5('test')", true)
        .unwrap_err();
    assert_eq!(err_text(&e), "Exception: test exception\n");
    let e = db
        .query_single(&mut h, b"SELECT my_udf_md5('test')", false, true)
        .unwrap_err();
    assert_eq!(err_text(&e), "Exception: test exception\n");
    let stmt = db
        .prepare(&mut h, b"SELECT my_udf_md5('test')")
        .unwrap()
        .unwrap();
    let e = stmt.execute(&mut h).unwrap_err();
    assert_eq!(err_text(&e), "Exception: test exception\n");
    assert!(h.log.is_empty());
}

#[test]
fn sqlite3_09_blob_bound_param_from_stream() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (id STRING, data BLOB)");
    let ins = db
        .prepare(&mut h, b"INSERT INTO test (id, data) VALUES (?, ?)")
        .unwrap()
        .unwrap();
    let res = Resource::new("stream", None);
    h.streams.insert(res.handle(), b"abcdefg\0hi".to_vec());
    assert!(ins
        .bind_value(ParamKey::Number(1), &s("a"), Some(SQLITE3_TEXT))
        .unwrap());
    assert!(ins
        .bind_param(
            ParamKey::Number(2),
            Value::Reference(PhpRef::new(Value::Resource(res))),
            Some(SQLITE3_BLOB)
        )
        .unwrap());
    ins.execute(&mut h).unwrap().unwrap();
    assert!(ins.close().unwrap());
    let r = db
        .query(
            &mut h,
            b"SELECT id, quote(data) AS data FROM test ORDER BY id ASC",
            true,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        rows(&mut h, &r, SQLITE3_NUM),
        "array(2) {\n  [0]=>\n  string(1) \"a\"\n  [1]=>\n  string(23) \"X'61626364656667006869'\"\n}\n"
    );
    // a non-stream resource fails
    let ins = db
        .prepare(&mut h, b"INSERT INTO test (id, data) VALUES (1, ?)")
        .unwrap()
        .unwrap();
    ins.bind_value(
        ParamKey::Number(1),
        &Value::Resource(Resource::new("gd", None)),
        Some(SQLITE3_BLOB),
    )
    .unwrap();
    h.at("SQLite3Stmt::execute");
    assert!(ins.execute(&mut h).unwrap().is_none());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3Stmt::execute(): Unable to read stream for parameter 1"]
    );
    // sqlite3_blob_bind_resource: an empty stream binds an empty blob
    let empty = Resource::new("stream", None);
    ins.bind_param(
        ParamKey::Number(1),
        Value::Reference(PhpRef::new(Value::Resource(empty))),
        Some(SQLITE3_BLOB),
    )
    .unwrap();
    assert!(ins.execute(&mut h).unwrap().is_some());
    // bug71049: binding as BLOB does not modify the variable
    let foo = PhpRef::new(s("alive!"));
    let st = db
        .prepare(&mut h, b"SELECT * FROM test WHERE id = ?")
        .unwrap()
        .unwrap();
    st.bind_param(
        ParamKey::Number(1),
        Value::Reference(foo.clone()),
        Some(SQLITE3_BLOB),
    )
    .unwrap();
    st.execute(&mut h).unwrap().unwrap();
    assert_eq!(dump(&foo.get()), "string(6) \"alive!\"\n");
}

#[test]
fn sqlite3_12_unfinalized_cleanup_and_bug66550() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    two_rows(&mut h, &db);
    let r = db
        .query(&mut h, b"SELECT * FROM test ORDER BY id ASC", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        mask_ints(&dump(&r.fetch_array(&mut h, SQLITE3_NUM).unwrap())),
        ROW_A
    );
    let stmt = db.prepare(&mut h, b"SELECT * FROM test").unwrap().unwrap();
    assert!(db.close(&mut h).unwrap());
    assert_eq!(
        err_text(&r.num_columns().unwrap_err()),
        "Error: The SQLite3Result object has not been correctly initialised or is already closed"
    );
    assert_eq!(
        err_text(&stmt.reset(&mut h).unwrap_err()),
        "Error: The SQLite3 object has not been correctly initialised or is already closed"
    );
    // bug69972: error functions after close
    assert_eq!(db.last_error_code().unwrap(), 0);
    assert_eq!(db.last_error_msg().unwrap().as_bytes(), b"");
}

#[test]
fn sqlite3_14_query_single() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    two_rows(&mut h, &db);
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT id FROM test WHERE id = 'a'", false, true)
                .unwrap()
        ),
        "string(1) \"a\"\n"
    );
    assert_eq!(
        mask_ints(&dump(
            &db.query_single(
                &mut h,
                b"SELECT id, time FROM test WHERE id = 'a'",
                true,
                true
            )
            .unwrap()
        )),
        "array(2) {\n  [\"id\"]=>\n  string(1) \"a\"\n  [\"time\"]=>\n  int(%d)\n}\n"
    );
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT id FROM test WHERE 0", false, true)
                .unwrap()
        ),
        "NULL\n"
    );
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT id FROM test WHERE 0", true, true)
                .unwrap()
        ),
        "array(0) {\n}\n"
    );
    assert_eq!(
        dump(&db.query_single(&mut h, b"", false, true).unwrap()),
        "bool(false)\n"
    );
    // unused return value: exec path, always false
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"INSERT INTO test VALUES (1, 'c')", false, false)
                .unwrap()
        ),
        "bool(false)\n"
    );
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT count(*) FROM test", false, true)
                .unwrap()
        ),
        "int(3)\n"
    );
}

#[test]
fn sqlite3_16_17_18_19_misc() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    // sqlite3_31_changes
    assert_eq!(db.changes().unwrap(), 0);
    two_rows(&mut h, &db);
    exec(&mut h, &db, "UPDATE test SET id = 'c'");
    assert_eq!(db.changes().unwrap(), 2);
    // sqlite3_24
    assert_eq!(db.last_insert_row_id().unwrap(), 2);
    // sqlite3_17
    let v = Value::Array(Sqlite3::version());
    assert_eq!(
        print_r(&v),
        format!(
            "Array\n(\n    [versionString] => 3.45.1\n    [versionNumber] => {}\n)\n",
            php_sqlite::sqlite_version_number()
        )
    );
    // sqlite3_19 column info
    let r = db
        .query(&mut h, b"SELECT * FROM test ORDER BY id ASC", true)
        .unwrap()
        .unwrap();
    assert_eq!(r.column_type(0).unwrap(), None);
    let mut out = String::new();
    while r
        .fetch_array(&mut h, SQLITE3_NUM)
        .unwrap()
        .as_array()
        .is_some()
    {
        for i in 0..r.num_columns().unwrap() {
            out.push_str(&format!(
                "Name: {} - Type: {}\n",
                r.column_name(i).unwrap().unwrap().to_string_lossy(),
                r.column_type(i).unwrap().unwrap()
            ));
        }
        // bug53463: out of range column name
        assert!(r.column_name(3).unwrap().is_none());
    }
    assert_eq!(
        out,
        "Name: time - Type: 1\nName: id - Type: 3\nName: time - Type: 1\nName: id - Type: 3\n"
    );
    // sqlite3_16: no rows
    let r = db
        .query(&mut h, b"SELECT * FROM test WHERE 0", true)
        .unwrap()
        .unwrap();
    assert_eq!(rows(&mut h, &r, SQLITE3_NUM), "");
    // empty SQL
    assert!(db.prepare(&mut h, b"").unwrap().is_none());
    assert!(db.query(&mut h, b"", true).unwrap().is_none());
}

#[test]
fn bug79294_column_type_after_done() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE foo (bar INT)");
    exec(&mut h, &db, "INSERT INTO foo VALUES (1)");
    let stmt = db.prepare(&mut h, b"SELECT * FROM foo").unwrap().unwrap();
    let res = stmt.execute(&mut h).unwrap().unwrap();
    assert!(res
        .fetch_array(&mut h, SQLITE3_BOTH)
        .unwrap()
        .as_array()
        .is_some());
    assert_eq!(res.column_type(0).unwrap(), Some(1));
    assert!(res
        .fetch_array(&mut h, SQLITE3_BOTH)
        .unwrap()
        .as_array()
        .is_none());
    assert_eq!(res.column_type(0).unwrap(), None);
    stmt.reset(&mut h).unwrap();
    assert!(res
        .fetch_array(&mut h, SQLITE3_BOTH)
        .unwrap()
        .as_array()
        .is_some());
    assert_eq!(res.column_type(0).unwrap(), Some(1));
    res.reset(&mut h).unwrap();
    assert!(res
        .fetch_array(&mut h, SQLITE3_BOTH)
        .unwrap()
        .as_array()
        .is_some());
    assert_eq!(res.column_type(0).unwrap(), Some(1));
}

#[test]
fn sqlite3_20_error_and_enable_exceptions() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.at("SQLite3::query");
    assert!(db
        .query(&mut h, b"SELECT * FROM non_existent_table", true)
        .unwrap()
        .is_none());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::query(): Unable to prepare statement: no such table: non_existent_table"]
    );
    assert_eq!(db.last_error_code().unwrap(), 1);
    assert_eq!(
        db.last_error_msg().unwrap().to_string_lossy(),
        "no such table: non_existent_table"
    );
    // sqlite3_enable_exceptions (unused return value takes the exec path)
    assert!(!db.enable_exceptions(&mut h, true).unwrap());
    let e = db
        .query(&mut h, b"SELECT * FROM non_existent_table", false)
        .unwrap_err();
    assert_eq!(
        err_text(&e),
        "SQLite3Exception: no such table: non_existent_table"
    );
    match &e {
        PhpError::Exception(x) => assert_eq!(x.code, 1),
        PhpError::Runtime(_) => panic!(),
    }
    h.at("SQLite3::enableExceptions");
    assert!(db.enable_exceptions(&mut h, false).unwrap());
    assert_eq!(
        h.take_log(),
        ["Deprecated: SQLite3::enableExceptions(): Use of warnings for SQLite3 is deprecated"]
    );
    h.at("SQLite3::query");
    assert!(db
        .query(&mut h, b"SELECT * FROM non_existent_table", false)
        .unwrap()
        .is_none());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::query(): no such table: non_existent_table"]
    );
    assert!(db.close(&mut h).unwrap());
}

#[test]
fn sqlite3_38_extended_error_and_toggle() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    db.query(
        &mut h,
        b"CREATE TABLE dog ( id INTEGER PRIMARY KEY, name TEXT, annoying INTEGER )",
        true,
    )
    .unwrap();
    db.query(
        &mut h,
        b"INSERT INTO dog VALUES (1, 'Annoying Dog', 1)",
        true,
    )
    .unwrap();
    assert_eq!(db.last_error_code().unwrap(), 0);
    h.at("SQLite3::query");
    assert!(db
        .query(
            &mut h,
            b"INSERT INTO dog VALUES (1, 'Annoying Dog', 1)",
            true
        )
        .unwrap()
        .is_none());
    assert_eq!(h.take_log(), ["Warning: SQLite3::query(): Unable to execute statement: UNIQUE constraint failed: dog.id"]);
    assert_eq!(db.last_error_code().unwrap(), 19);
    assert_eq!(db.last_extended_error_code().unwrap(), 1555);
    assert!(db.enable_extended_result_codes(true).unwrap());
    db.query(
        &mut h,
        b"INSERT INTO DOG VALUES (1, 'Annoying Dog', 1)",
        true,
    )
    .unwrap();
    assert_eq!(db.last_error_code().unwrap(), 1555);
}

#[test]
fn sqlite3_38_backup() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (a, b);");
    exec(&mut h, &db, "INSERT INTO test VALUES (42, 'php');");
    let db2 = mem(&mut h);
    assert!(db.backup(&mut h, &db2, b"main", b"main").unwrap());
    assert_eq!(
        dump(
            &db2.query_single(&mut h, b"SELECT COUNT(*) FROM sqlite_master;", false, true)
                .unwrap()
        ),
        "int(1)\n"
    );
    assert_eq!(
        dump(
            &db2.query_single(&mut h, b"SELECT a FROM test;", false, true)
                .unwrap()
        ),
        "int(42)\n"
    );
    assert_eq!(
        dump(
            &db2.query_single(&mut h, b"SELECT b FROM test;", false, true)
                .unwrap()
        ),
        "string(3) \"php\"\n"
    );
    db2.close(&mut h).unwrap();
    let db2 = mem(&mut h);
    exec(&mut h, &db, "BEGIN EXCLUSIVE;");
    h.at("SQLite3::backup");
    assert!(!db.backup(&mut h, &db2, b"main", b"main").unwrap());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::backup(): Backup failed: source database is busy"]
    );
    // closed destination
    db2.close(&mut h).unwrap();
    assert_eq!(
        err_text(&db.backup(&mut h, &db2, b"main", b"main").unwrap_err()),
        "Error: The SQLite3 object has not been correctly initialised or is already closed"
    );
}

#[test]
fn sqlite3_40_setauthorizer() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    db.enable_exceptions(&mut h, true).unwrap();
    let only_select = h.func("only_select", |_, a| {
        Ok(Value::Int(i64::from(a[0].as_int() != Some(21))))
    });
    assert!(db.set_authorizer(Some(only_select)).unwrap());
    assert_eq!(
        dump(&db.query_single(&mut h, b"SELECT 1;", false, true).unwrap()),
        "int(1)\n"
    );
    let e = db
        .query_single(&mut h, b"CREATE TABLE test (a, b);", false, true)
        .unwrap_err();
    assert_eq!(
        err_text(&e),
        "SQLite3Exception: Unable to prepare statement: not authorized"
    );
    db.set_authorizer(None).unwrap();
    exec(
        &mut h,
        &db,
        "CREATE TABLE test (a); INSERT INTO test VALUES (42);",
    );
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT a FROM test;", false, true)
                .unwrap()
        ),
        "int(42)\n"
    );
    // argument marshalling (constant names flipped: last wins)
    let names: std::collections::HashMap<i64, &str> = SQLITE3_CLASS_CONSTANTS
        .iter()
        .map(|(n, v)| (*v, *n))
        .collect();
    let logger = h.func("logger", move |h, a| {
        let rest: Vec<String> = a[1..]
            .iter()
            .map(|v| v.as_str().map(ZStr::to_string_lossy).unwrap_or_default())
            .collect();
        h.log.push(format!(
            "{} {}",
            names[&a[0].as_int().unwrap()],
            rest.join(",")
        ));
        Ok(Value::Int(0))
    });
    db.set_authorizer(Some(logger)).unwrap();
    exec(&mut h, &db, "SELECT * FROM test WHERE a = 42;");
    exec(&mut h, &db, "DROP TABLE test;");
    assert_eq!(
        h.take_log(),
        [
            "SELECT ,,,",
            "READ test,a,main,",
            "READ test,a,main,",
            "DELETE sqlite_master,,main,",
            "DROP_TABLE test,,main,",
            "DELETE test,,main,",
            "DELETE sqlite_master,,main,",
            "READ sqlite_master,tbl_name,main,",
            "READ sqlite_master,type,main,",
            "UPDATE sqlite_master,rootpage,main,",
            "READ sqlite_master,rootpage,main,",
        ]
    );
    // invalid return values chain the authorizer error as previous
    let fail = h.func("fail", |_, _| Ok(Value::from("FAIL")));
    db.set_authorizer(Some(fail)).unwrap();
    let e = db
        .query_single(&mut h, b"SELECT 1;", false, true)
        .unwrap_err();
    assert_eq!(
        err_text(&e),
        "SQLite3Exception: Unable to prepare statement: not authorized"
    );
    assert_eq!(
        err_text(e.previous().unwrap()),
        "SQLite3Exception: The authorizer callback returned an invalid type: expected int"
    );
    let big = h.func("big", |_, _| Ok(Value::Int(4200)));
    db.set_authorizer(Some(big)).unwrap();
    let e = db
        .query_single(&mut h, b"SELECT 1;", false, true)
        .unwrap_err();
    assert_eq!(
        e.previous().unwrap().message(),
        Some("The authorizer callback returned an invalid value: 4200")
    );
}

#[test]
fn authorizer_warning_mode_and_gh22122() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let fail = h.func("fail", |_, _| Ok(Value::from("FAIL")));
    db.set_authorizer(Some(fail)).unwrap();
    h.at("SQLite3::querySingle");
    assert_eq!(
        dump(&db.query_single(&mut h, b"SELECT 1;", false, true).unwrap()),
        "bool(false)\n"
    );
    assert_eq!(
        h.take_log(),
        [
            "Warning: SQLite3::querySingle(): The authorizer callback returned an invalid type: expected int",
            "Warning: SQLite3::querySingle(): Unable to prepare statement: not authorized",
        ]
    );
    // gh22122: disabling the authorizer from inside itself
    let db2 = db.clone();
    let disable = h.func("disable", move |h, _| {
        db2.set_authorizer(None)
            .map_err(|e| RuntimeError::Error(e.to_string()))?;
        h.log.push("method: alive".into());
        Ok(Value::Int(0))
    });
    db.set_authorizer(Some(disable)).unwrap();
    exec(&mut h, &db, "SELECT 1");
    exec(&mut h, &db, "SELECT 2");
    assert_eq!(h.take_log(), ["method: alive"]);
}

#[test]
fn bug81742_and_gh9032_open_basedir_attach() {
    let mut h = TestHost::new();
    h.open_basedir = Some(b"/nonexistent-basedir".to_vec());
    let db = mem(&mut h);
    h.at("SQLite3::query");
    assert!(db
        .query(&mut h, b"ATTACH 'file:..%2ffoo.php' as db2;", false)
        .unwrap()
        .is_none());
    assert_eq!(h.take_log(), ["Warning: SQLite3::query(): not authorized"]);
    db.enable_exceptions(&mut h, true).unwrap();
    exec(&mut h, &db, "attach database ':memory:' AS \"db1\"");
    exec(&mut h, &db, "create table db1.r (id int)");
    let e = db
        .prepare(&mut h, b"attach database :a AS \"db2\"")
        .unwrap_err();
    assert_eq!(
        err_text(&e),
        "SQLite3Exception: Unable to prepare statement: not authorized"
    );
}

#[test]
fn close_from_callbacks() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let d = db.clone();
    let boom = h.func("boom", move |h, _| {
        if let Err(e) = d.close(h) {
            h.log.push(err_text(&e));
        }
        Ok(Value::Int(1))
    });
    db.create_function(b"boom", boom, -1, 0).unwrap();
    let stmt = db.prepare(&mut h, b"SELECT boom()").unwrap().unwrap();
    let res = stmt.execute(&mut h).unwrap().unwrap();
    assert_eq!(
        dump(&res.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "array(1) {\n  [0]=>\n  int(1)\n}\n"
    );
    res.finalize(&mut h).unwrap();
    assert_eq!(
        h.take_log(),
        ["Error: Cannot close SQLite3 database while inside a callback"; 2]
    );
    // collation and authorizer
    let d = db.clone();
    let cb = h.func("cb", move |h, a| {
        if let Err(e) = d.close(h) {
            h.log.push(err_text(&e));
        }
        Ok(Value::Int(
            i64::from(a[0].as_str().unwrap().as_bytes() > a[1].as_str().unwrap().as_bytes()) * 2
                - 1,
        ))
    });
    db.create_collation(b"CB", cb).unwrap();
    exec(
        &mut h,
        &db,
        "CREATE TABLE t (s TEXT); INSERT INTO t VALUES ('b'), ('a')",
    );
    let res = db
        .prepare(&mut h, b"SELECT s FROM t ORDER BY s COLLATE CB")
        .unwrap()
        .unwrap()
        .execute(&mut h)
        .unwrap()
        .unwrap();
    let out = rows(&mut h, &res, SQLITE3_NUM);
    assert!(out.find("\"a\"").unwrap() < out.find("\"b\"").unwrap());
    assert!(h
        .take_log()
        .iter()
        .all(|l| l == "Error: Cannot close SQLite3 database while inside a callback"));
    let d = db.clone();
    let auth = h.func("auth", move |h, _| {
        if let Err(e) = d.close(h) {
            h.log.push(err_text(&e));
        }
        Ok(Value::Int(0))
    });
    db.set_authorizer(Some(auth)).unwrap();
    db.prepare(&mut h, b"SELECT 1").unwrap().unwrap();
    assert_eq!(
        h.take_log()[0],
        "Error: Cannot close SQLite3 database while inside a callback"
    );
    db.set_authorizer(None).unwrap();
    assert!(db.close(&mut h).unwrap());
}

#[test]
fn close_from_aggregate_callback() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (1); INSERT INTO t VALUES (2)",
    );
    let d = db.clone();
    let reported = std::cell::Cell::new(false);
    let step = h.func("step", move |h, a| {
        if !reported.replace(true) {
            if let Err(e) = d.close(h) {
                h.log.push(err_text(&e));
            }
        }
        Ok(Value::Int(
            a[0].as_int().unwrap_or(0) + a[2].as_int().unwrap(),
        ))
    });
    let fin = h.func("fin", |_, a| Ok(a[0].clone()));
    db.create_aggregate(b"agg", step, fin, 1).unwrap();
    let res = db
        .prepare(&mut h, b"SELECT agg(a) FROM t")
        .unwrap()
        .unwrap()
        .execute(&mut h)
        .unwrap()
        .unwrap();
    assert_eq!(
        dump(&res.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "array(1) {\n  [0]=>\n  int(3)\n}\n"
    );
    assert_eq!(
        h.take_log(),
        ["Error: Cannot close SQLite3 database while inside a callback"]
    );
}

#[test]
fn sqlite3_30_blobopen_and_streams() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (id STRING, data BLOB)");
    let ins = db
        .prepare(&mut h, b"INSERT INTO test (id, data) VALUES (?, ?)")
        .unwrap()
        .unwrap();
    ins.bind_value(ParamKey::Number(1), &s("a"), Some(SQLITE3_TEXT))
        .unwrap();
    ins.bind_value(ParamKey::Number(2), &s("TEST TEST"), Some(SQLITE3_BLOB))
        .unwrap();
    ins.execute(&mut h).unwrap();
    ins.close().unwrap();
    php_streams::diag::take();
    let mut st: Stream = db
        .open_blob(&mut h, b"test", b"data", 1, b"main", SQLITE3_OPEN_READONLY)
        .unwrap()
        .unwrap()
        .into_stream();
    assert_eq!(st.mode, "rb");
    assert_eq!(st.read_vec(100).unwrap(), b"TEST TEST");
    assert!(st.write(b"ABCD").is_err());
    let d = php_streams::diag::take();
    assert_eq!(
        d[0].text(),
        "Can't write to blob stream: is open as read only"
    );
    drop(st);
    let blob = db
        .open_blob(&mut h, b"test", b"data", 1, b"main", SQLITE3_OPEN_READWRITE)
        .unwrap()
        .unwrap();
    assert_eq!(blob.size(), 9);
    let mut st = blob.into_stream();
    assert_eq!(st.mode, "r+b");
    assert_eq!(st.write(b"ABCD"), Ok(4));
    assert_eq!(st.seek(0, 0), 0);
    assert_eq!(st.read_vec(100).unwrap(), b"ABCD TEST");
    assert!(st.write(b"ABCD ABCD ABCD").is_err());
    assert_eq!(
        php_streams::diag::take()[0].text(),
        "It is not possible to increase the size of a BLOB"
    );
    // gh20964: seeks with PHP_INT_MIN fail
    assert_eq!(st.seek(i64::MIN, 2), -1);
    assert_eq!(st.seek(0, 0), 0);
    assert_eq!(st.seek(i64::MIN, 1), -1);
    drop(st);
    // invalid blob
    h.at("SQLite3::openBlob");
    assert!(db
        .open_blob(&mut h, b"test", b"nope", 1, b"main", SQLITE3_OPEN_READONLY)
        .unwrap()
        .is_none());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::openBlob(): Unable to open blob: no such column: \"nope\""]
    );
}

#[test]
fn sqlite3_close_blob_stream() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (data BLOB)");
    exec(&mut h, &db, "INSERT INTO test (data) VALUES (x'34323432')");
    let mut st = db
        .open_blob(&mut h, b"test", b"data", 1, b"main", SQLITE3_OPEN_READONLY)
        .unwrap()
        .unwrap()
        .into_stream();
    h.at("SQLite3::close");
    assert!(!db.close(&mut h).unwrap());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::close(): Unable to close database: unable to close due to unfinalized statements or unfinished backups"]
    );
    drop(db);
    assert_eq!(st.read_vec(4).unwrap(), b"4242");
}

#[test]
fn escape_string() {
    assert_eq!(
        Sqlite3::escape_string(b"test''%").unwrap().as_bytes(),
        b"test''''%"
    );
    assert_eq!(Sqlite3::escape_string(b"").unwrap().as_bytes(), b"");
    assert_eq!(Sqlite3::escape_string(b"ok").unwrap().as_bytes(), b"ok");
    for bad in [&b"\0"[..], b"a\0b", b"foo\0\0bar"] {
        assert_eq!(
            err_text(&Sqlite3::escape_string(bad).unwrap_err()),
            "ValueError: SQLite3::escapeString(): Argument #1 ($string) must not contain any null bytes"
        );
    }
}

#[test]
fn open_errors_and_uninitialised() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    assert_eq!(
        err_text(&db.open(&mut h, b":memory:", RWC, b"").unwrap_err()),
        "Exception: Already initialised DB Object"
    );
    let e = Sqlite3::construct(&mut h, b"/nonexistent-dir/x/y.db", RWC, b"").unwrap_err();
    assert_eq!(
        err_text(&e),
        "Exception: Unable to open database: unable to open database file"
    );
    // sqlite3_21_security
    h.open_basedir = Some(b"/nonexistent-basedir".to_vec());
    h.at("SQLite3::__construct");
    let e = Sqlite3::construct(&mut h, b"/tmp/bad.db", RWC, b"").unwrap_err();
    assert_eq!(
        err_text(&e),
        "Exception: open_basedir prohibits opening /tmp/bad.db"
    );
    assert_eq!(h.take_log().len(), 1);
    h.open_basedir = None;
    // read-only open of a missing file
    let e = Sqlite3::construct(
        &mut h,
        b"/tmp/php-sqlite-test-missing.db",
        SQLITE3_OPEN_READONLY,
        b"",
    )
    .unwrap_err();
    assert_eq!(
        err_text(&e),
        "Exception: Unable to open database: unable to open database file"
    );
    // sqlite3_open_empty_string: a temporary database
    let tmp = Sqlite3::construct(&mut h, b"", RWC, b"").unwrap();
    assert!(tmp.is_open());
    // newInstanceWithoutConstructor()
    let raw = Sqlite3::new();
    let msg = "Error: The SQLite3 object has not been correctly initialised or is already closed";
    let f = h.func("f", |_, _| Ok(Value::Null));
    assert_eq!(
        err_text(&raw.create_function(b"x", f.clone(), -1, 0).unwrap_err()),
        msg
    );
    assert_eq!(
        err_text(
            &raw.create_aggregate(b"x", f.clone(), f.clone(), -1)
                .unwrap_err()
        ),
        msg
    );
    assert_eq!(
        err_text(&raw.create_collation(b"x", f.clone()).unwrap_err()),
        msg
    );
    assert_eq!(err_text(&raw.set_authorizer(Some(f)).unwrap_err()), msg);
    assert_eq!(err_text(&raw.last_error_code().unwrap_err()), msg);
    assert_eq!(err_text(&raw.exec(&mut h, b"SELECT 1").unwrap_err()), msg);
    assert!(raw.close(&mut h).unwrap());
    // sqlite3_01_open with a file name
    let path = std::env::temp_dir().join(format!("php-sqlite-test-{}.db", std::process::id()));
    let p = path.to_string_lossy().into_owned();
    let fdb = Sqlite3::construct(&mut h, p.as_bytes(), RWC, b"").unwrap();
    exec(&mut h, &fdb, "CREATE TABLE x (a)");
    assert!(fdb.close(&mut h).unwrap());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn load_extension_rules() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    h.at("SQLite3::loadExtension");
    assert!(!db.load_extension(&mut h, b"").unwrap());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::loadExtension(): SQLite Extensions are disabled"]
    );
    let dir = std::env::temp_dir();
    h.extension_dir = Some(dir.to_string_lossy().as_bytes().to_vec());
    assert_eq!(
        err_text(&db.load_extension(&mut h, b"").unwrap_err()),
        "ValueError: SQLite3::loadExtension(): Argument #1 ($name) must not be empty"
    );
    assert!(!db.load_extension(&mut h, b"does-not-exist.so").unwrap());
    assert_eq!(
        h.take_log(),
        [format!(
            "Warning: SQLite3::loadExtension(): Unable to load extension at '{}/does-not-exist.so'",
            dir.to_string_lossy()
        )]
    );
    let f = dir.join(format!("php-sqlite-myext-{}.txt", std::process::id()));
    std::fs::write(&f, b"not a library").unwrap();
    let name = f.file_name().unwrap().to_string_lossy().into_owned();
    assert!(!db.load_extension(&mut h, name.as_bytes()).unwrap());
    let log = h.take_log();
    assert!(
        log[0].starts_with("Warning: SQLite3::loadExtension(): "),
        "{log:?}"
    );
    std::fs::remove_file(f).unwrap();
}

#[test]
fn sqlite3_35_readonly_busy_paramcount() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    two_rows(&mut h, &db);
    let st = db
        .prepare(&mut h, b"SELECT * FROM test WHERE id = ? ORDER BY id ASC")
        .unwrap()
        .unwrap();
    assert!(st.read_only().unwrap());
    assert_eq!(st.param_count().unwrap(), 1);
    assert!(!db
        .prepare(&mut h, b"UPDATE test SET id = 'c' WHERE id = ?")
        .unwrap()
        .unwrap()
        .read_only()
        .unwrap());
    assert!(!db
        .prepare(&mut h, b"DELETE FROM test")
        .unwrap()
        .unwrap()
        .read_only()
        .unwrap());
    for (q, n) in [
        ("SELECT * FROM test WHERE time = 2", 0),
        ("SELECT * FROM test WHERE time = :id AND id = :name", 2),
        ("SELECT * FROM test WHERE time = 1 AND id = :name", 1),
    ] {
        assert_eq!(
            db.prepare(&mut h, q.as_bytes())
                .unwrap()
                .unwrap()
                .param_count()
                .unwrap(),
            n
        );
    }
    // sqlite3_stmt_busy
    let st = db.prepare(&mut h, b"SELECT id FROM test").unwrap().unwrap();
    assert!(!st.busy().unwrap());
    let r = st.execute(&mut h).unwrap().unwrap();
    r.fetch_array(&mut h, SQLITE3_BOTH).unwrap();
    assert!(st.busy().unwrap());
    // a statement-less SQL text yields a NULL statement
    let empty = db.prepare(&mut h, b"-- nothing").unwrap().unwrap();
    assert_eq!(
        err_text(&empty.param_count().unwrap_err()),
        "Error: The SQLite3Stmt object has not been correctly initialised or is already closed"
    );
    h.at("SQLite3Stmt::execute");
    assert!(empty.execute(&mut h).unwrap().is_none());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3Stmt::execute(): Unable to execute statement: out of memory"]
    );
}

#[test]
fn sqlite3stmt_getsql_and_bug77051() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    db.enable_exceptions(&mut h, true).unwrap();
    let st = db.prepare(&mut h, b"SELECT :a, :b, ?;").unwrap().unwrap();
    st.bind_value(ParamKey::Name(b":a"), &Value::Int(42), None)
        .unwrap();
    st.bind_value(ParamKey::Name(b":b"), &s("php"), None)
        .unwrap();
    st.bind_value(ParamKey::Number(3), &Value::Int(43), None)
        .unwrap();
    assert_eq!(
        st.get_sql(&mut h, false)
            .unwrap()
            .unwrap()
            .to_string_lossy(),
        "SELECT :a, :b, ?;"
    );
    assert_eq!(
        st.get_sql(&mut h, true).unwrap().unwrap().to_string_lossy(),
        "SELECT 42, 'php', 43;"
    );
    st.bind_value(ParamKey::Name(b":a"), &s("TEST"), None)
        .unwrap();
    st.bind_value(ParamKey::Name(b":b"), &s("!!!"), None)
        .unwrap();
    st.bind_value(ParamKey::Number(3), &Value::Int(40), None)
        .unwrap();
    assert_eq!(
        st.get_sql(&mut h, true).unwrap().unwrap().to_string_lossy(),
        "SELECT 'TEST', '!!!', 40;"
    );
    // bug77051: re-execution rebinding and conversion through the reference
    let st = db.prepare(&mut h, b"SELECT :a, :b, ?;").unwrap().unwrap();
    let a = PhpRef::new(Value::Int(42));
    st.bind_param(
        ParamKey::Name(b":a"),
        Value::Reference(a.clone()),
        Some(SQLITE3_INTEGER),
    )
    .unwrap();
    st.bind_value(ParamKey::Name(b":b"), &s("php"), None)
        .unwrap();
    st.bind_value(ParamKey::Name(b":b"), &s("PHP"), None)
        .unwrap();
    st.bind_value(ParamKey::Number(3), &Value::Int(424_242), None)
        .unwrap();
    let r = st.execute(&mut h).unwrap().unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "array(3) {\n  [0]=>\n  int(42)\n  [1]=>\n  string(3) \"PHP\"\n  [2]=>\n  int(424242)\n}\n"
    );
    a.set(s("TEST"));
    let r = st.execute(&mut h).unwrap().unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "array(3) {\n  [0]=>\n  int(0)\n  [1]=>\n  string(3) \"PHP\"\n  [2]=>\n  int(424242)\n}\n"
    );
    assert_eq!(dump(&a.get()), "int(0)\n");
    a.set(Value::Int(5_252_552));
    st.bind_value(ParamKey::Name(b":b"), &s("TEST"), None)
        .unwrap();
    st.bind_value(ParamKey::Number(3), &s("!!!"), None).unwrap();
    let r = st.execute(&mut h).unwrap().unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "array(3) {\n  [0]=>\n  int(5252552)\n  [1]=>\n  string(4) \"TEST\"\n  [2]=>\n  string(3) \"!!!\"\n}\n"
    );
}

#[test]
fn bind_type_inference_bug68849_and_floats() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (a INTEGER, b TEXT, c REAL);INSERT INTO test VALUES (1, 'hello', 3.14);INSERT INTO test VALUES (3, 'world', 3.15);INSERT INTO test VALUES (0, '42', 0.42);");
    let q = |h: &mut TestHost, sql: &[u8], v: Value| {
        let st = db.prepare(h, sql).unwrap().unwrap();
        st.bind_value(ParamKey::Number(1), &v, None).unwrap();
        dump(
            &st.execute(h)
                .unwrap()
                .unwrap()
                .fetch_array(h, SQLITE3_ASSOC)
                .unwrap(),
        )
    };
    let row1 = "array(3) {\n  [\"a\"]=>\n  int(1)\n  [\"b\"]=>\n  string(5) \"hello\"\n  [\"c\"]=>\n  float(3.14)\n}\n";
    assert_eq!(
        q(
            &mut h,
            b"SELECT * FROM test WHERE (a+2) = ?;",
            Value::Int(3)
        ),
        row1
    );
    assert_eq!(
        q(
            &mut h,
            b"SELECT * FROM test WHERE a = ?;",
            Value::Bool(true)
        ),
        row1
    );
    assert_eq!(
        q(&mut h, b"SELECT * FROM test WHERE a = ?;", Value::Bool(false)),
        "array(3) {\n  [\"a\"]=>\n  int(0)\n  [\"b\"]=>\n  string(2) \"42\"\n  [\"c\"]=>\n  float(0.42)\n}\n"
    );
    assert_eq!(
        q(&mut h, b"SELECT * FROM test WHERE c = ?;", Value::Float(3.15)),
        "array(3) {\n  [\"a\"]=>\n  int(3)\n  [\"b\"]=>\n  string(5) \"world\"\n  [\"c\"]=>\n  float(3.15)\n}\n"
    );
    // bug76665: explicit float type
    exec(&mut h, &db, "CREATE TABLE foo (bar REAL)");
    let st = db
        .prepare(&mut h, b"INSERT INTO foo VALUES (:bar)")
        .unwrap()
        .unwrap();
    st.bind_value(
        ParamKey::Name(b":bar"),
        &Value::Int(17),
        Some(SQLITE3_FLOAT),
    )
    .unwrap();
    st.execute(&mut h).unwrap();
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT bar FROM foo LIMIT 1", false, true)
                .unwrap()
        ),
        "float(17)\n"
    );
    // bindParam without type binds as TEXT; null binds NULL; unknown types fail
    let st = db
        .prepare(&mut h, b"SELECT typeof(?), typeof(?)")
        .unwrap()
        .unwrap();
    st.bind_param(
        ParamKey::Number(1),
        Value::Reference(PhpRef::new(Value::Int(5))),
        None,
    )
    .unwrap();
    st.bind_value(ParamKey::Number(2), &Value::Null, Some(SQLITE3_INTEGER))
        .unwrap();
    assert_eq!(
        dump(
            &st.execute(&mut h)
                .unwrap()
                .unwrap()
                .fetch_array(&mut h, SQLITE3_NUM)
                .unwrap()
        ),
        "array(2) {\n  [0]=>\n  string(4) \"text\"\n  [1]=>\n  string(4) \"null\"\n}\n"
    );
    st.bind_value(ParamKey::Number(1), &Value::Int(1), Some(99))
        .unwrap();
    h.at("SQLite3Stmt::execute");
    assert!(st.execute(&mut h).unwrap().is_none());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3Stmt::execute(): Unknown parameter type: 99 for parameter 1"]
    );
}

#[test]
fn exception_from_to_string() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE t(id int, v varchar(255))");
    let st = db
        .prepare(&mut h, b"INSERT INTO t VALUES(:i, :v)")
        .unwrap()
        .unwrap();
    st.bind_value(ParamKey::Name(b"i"), &Value::Int(1234), None)
        .unwrap();
    st.bind_value(ParamKey::Name(b"v"), &throws_object(), None)
        .unwrap();
    assert_eq!(
        err_text(&st.execute(&mut h).unwrap_err()),
        "Exception: Sorry"
    );
    assert_eq!(
        err_text(&st.execute(&mut h).unwrap_err()),
        "Exception: Sorry"
    );
    let r = db.query(&mut h, b"SELECT * FROM t", true).unwrap().unwrap();
    assert_eq!(rows(&mut h, &r, SQLITE3_ASSOC), "");
}

#[test]
fn bug70628_clear_then_rebind() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(
        &mut h,
        &db,
        "CREATE TABLE Dogs (Id INTEGER PRIMARY KEY, Breed TEXT, Name TEXT, Age INTEGER)",
    );
    let st = db
        .prepare(
            &mut h,
            b"INSERT INTO Dogs (Breed, Name, Age)  VALUES (:breed,:name,:age)",
        )
        .unwrap()
        .unwrap();
    st.bind_value(ParamKey::Name(b":breed"), &s("canis"), Some(SQLITE3_TEXT))
        .unwrap();
    st.bind_value(ParamKey::Name(b":name"), &s("jack"), Some(SQLITE3_TEXT))
        .unwrap();
    st.bind_value(
        ParamKey::Name(b":age"),
        &Value::Int(7),
        Some(SQLITE3_INTEGER),
    )
    .unwrap();
    st.execute(&mut h).unwrap();
    st.clear(&mut h).unwrap();
    st.reset(&mut h).unwrap();
    st.bind_value(ParamKey::Name(b":breed"), &s("russel"), Some(SQLITE3_TEXT))
        .unwrap();
    st.bind_value(
        ParamKey::Name(b":age"),
        &Value::Int(3),
        Some(SQLITE3_INTEGER),
    )
    .unwrap();
    st.execute(&mut h).unwrap();
    let r = db
        .query(&mut h, b"SELECT * FROM Dogs", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        rows(&mut h, &r, SQLITE3_ASSOC),
        "array(4) {\n  [\"Id\"]=>\n  int(1)\n  [\"Breed\"]=>\n  string(5) \"canis\"\n  [\"Name\"]=>\n  string(4) \"jack\"\n  [\"Age\"]=>\n  int(7)\n}\n\
array(4) {\n  [\"Id\"]=>\n  int(2)\n  [\"Breed\"]=>\n  string(6) \"russel\"\n  [\"Name\"]=>\n  NULL\n  [\"Age\"]=>\n  int(3)\n}\n"
    );
}

#[test]
fn fetch_all_and_duplicate_names() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    db.query(
        &mut h,
        b"CREATE TABLE users (id INTEGER NOT NULL, num INTEGER NOT NULL, PRIMARY KEY(id))",
        true,
    )
    .unwrap();
    db.query(&mut h, b"insert into users (id, num) values (1, 1)", true)
        .unwrap();
    db.query(&mut h, b"insert into users (id, num) values (2, 2)", true)
        .unwrap();
    let st = db
        .query(&mut h, b"SELECT * FROM users", true)
        .unwrap()
        .unwrap();
    let all = st.fetch_all(&mut h, SQLITE3_BOTH).unwrap();
    st.reset(&mut h).unwrap();
    let mut each = String::new();
    loop {
        let r = st.fetch_array(&mut h, SQLITE3_BOTH).unwrap();
        if r.as_array().is_none() {
            break;
        }
        each.push_str(&dump(&r));
    }
    let all_dump = dump(&all);
    assert!(all_dump.starts_with("array(2) {\n  [0]=>\n  array(4) {\n    [0]=>\n    int(1)\n    [\"id\"]=>\n    int(1)\n    [1]=>\n    int(1)\n    [\"num\"]=>\n    int(1)\n  }\n"));
    assert_eq!(each.matches("array(4)").count(), 2);
    st.reset(&mut h).unwrap();
    assert_eq!(
        dump(&st.fetch_all(&mut h, SQLITE3_ASSOC).unwrap()),
        "array(2) {\n  [0]=>\n  array(2) {\n    [\"id\"]=>\n    int(1)\n    [\"num\"]=>\n    int(1)\n  }\n  [1]=>\n  array(2) {\n    [\"id\"]=>\n    int(2)\n    [\"num\"]=>\n    int(2)\n  }\n}\n"
    );
    // gh11451: duplicate column names, last wins
    let r = db
        .query(&mut h, b"SELECT 0 AS dummy, 1 AS key, 2 AS key", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_ASSOC).unwrap()),
        "array(2) {\n  [\"dummy\"]=>\n  int(0)\n  [\"key\"]=>\n  int(2)\n}\n"
    );
    // numeric column names become integer keys
    let r = db
        .query(&mut h, b"SELECT 5 AS \"1\"", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_ASSOC).unwrap()),
        "array(1) {\n  [1]=>\n  int(5)\n}\n"
    );
    // bug73333: 64-bit extremes stay integers; text with NUL bytes
    exec(
        &mut h,
        &db,
        &format!(
            "CREATE TABLE foo (bar INT); INSERT INTO foo VALUES ({}); INSERT INTO foo VALUES ({})",
            i64::MIN,
            i64::MAX
        ),
    );
    let r = db
        .query(&mut h, b"SELECT bar FROM foo", true)
        .unwrap()
        .unwrap();
    assert_eq!(rows(&mut h, &r, SQLITE3_NUM).matches("int(").count(), 2);
    exec(&mut h, &db, "CREATE TABLE messages (content TEXT)");
    let ins = db
        .prepare(&mut h, b"INSERT INTO messages (content) VALUES (:content)")
        .unwrap()
        .unwrap();
    ins.bind_value(
        ParamKey::Name(b":content"),
        &s("with\0null"),
        Some(SQLITE3_TEXT),
    )
    .unwrap();
    ins.execute(&mut h).unwrap();
    let r = db
        .query(&mut h, b"SELECT * FROM messages", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        rows(&mut h, &r, SQLITE3_ASSOC),
        "array(1) {\n  [\"content\"]=>\n  string(9) \"with\0null\"\n}\n"
    );
}

#[test]
fn rename_column_cache_and_errors() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE tbl (orig text)");
    exec(&mut h, &db, "insert into tbl values ('one'), ('two')");
    let r1 = db
        .prepare(&mut h, b"select * from tbl")
        .unwrap()
        .unwrap()
        .execute(&mut h)
        .unwrap()
        .unwrap();
    let r2 = db
        .prepare(&mut h, b"select * from tbl")
        .unwrap()
        .unwrap()
        .execute(&mut h)
        .unwrap()
        .unwrap();
    let key = |h: &mut TestHost, r: &Sqlite3Result| {
        let row = r.fetch_array(h, SQLITE3_ASSOC).unwrap();
        row.as_array()
            .unwrap()
            .first()
            .unwrap()
            .0
            .to_zstr()
            .to_string_lossy()
    };
    assert_eq!(key(&mut h, &r1), "orig");
    assert_eq!(key(&mut h, &r2), "orig");
    exec(&mut h, &db, "alter table tbl rename column orig to changed");
    r1.reset(&mut h).unwrap();
    assert_eq!(key(&mut h, &r1), "changed");
    assert_eq!(key(&mut h, &r2), "orig");
    // gh20699
    h.at("SQLite3Result::fetchArray");
    let r = db
        .prepare(&mut h, b"BEGIN;")
        .unwrap()
        .unwrap()
        .execute(&mut h)
        .unwrap()
        .unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_BOTH).unwrap()),
        "bool(false)\n"
    );
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3Result::fetchArray(): Unable to execute statement: cannot start a transaction within a transaction"]
    );
}

#[test]
fn gh22051_reset_reports_errors() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    let sql = b"SELECT abs(x) FROM (SELECT 1 AS x UNION ALL SELECT -9223372036854775807 - 1)";
    let r = db.query(&mut h, sql, true).unwrap().unwrap();
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "array(1) {\n  [0]=>\n  int(1)\n}\n"
    );
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_NUM).unwrap()),
        "bool(false)\n"
    );
    h.take_log();
    h.at("SQLite3Result::reset");
    assert!(!r.reset(&mut h).unwrap());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3Result::reset(): Statement reset reported an error: integer overflow"]
    );
    let st = db.prepare(&mut h, sql).unwrap().unwrap();
    let r = st.execute(&mut h).unwrap().unwrap();
    r.fetch_array(&mut h, SQLITE3_NUM).unwrap();
    r.fetch_array(&mut h, SQLITE3_NUM).unwrap();
    h.take_log();
    h.at("SQLite3Result::finalize");
    assert!(r.finalize(&mut h).unwrap());
    assert_eq!(h.take_log(), ["Warning: SQLite3Result::finalize(): Statement reset reported an error: integer overflow"]);
}

#[test]
fn defensive_and_dqs() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test (a, b);");
    exec(&mut h, &db, "PRAGMA writable_schema = ON;");
    h.at("SQLite3::querySingle");
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"DELETE FROM sqlite_master;", false, true)
                .unwrap()
        ),
        "bool(false)\n"
    );
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::querySingle(): Unable to prepare statement: table sqlite_master may not be modified"]
    );
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT COUNT(*) FROM sqlite_master;", false, true)
                .unwrap()
        ),
        "int(1)\n"
    );
    // sqlite3_dqs: double-quoted string literals are accepted by this library build
    exec(&mut h, &db, "CREATE TABLE t2 (s1 VARCHAR(255), s2 VARCHAR(255)); INSERT INTO t2 VALUES ('test', \"test\")");
}

#[test]
fn explain_modes() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    exec(&mut h, &db, "CREATE TABLE test_explain (a string);");
    let st = db
        .prepare(
            &mut h,
            b"INSERT INTO test_explain VALUES ('first insert'), ('second_insert')",
        )
        .unwrap()
        .unwrap();
    assert!(st.set_explain(1).unwrap());
    assert_eq!(st.explain().unwrap(), 1);
    let r = st.execute(&mut h).unwrap().unwrap();
    let all = dump(&r.fetch_all(&mut h, SQLITE3_ASSOC).unwrap());
    assert!(all.contains("[\"opcode\"]=>\n    string(4) \"Init\""));
    assert!(all.contains("string(12) \"first insert\""));
    let st = db
        .prepare(&mut h, b"SELECT * FROM test_explain")
        .unwrap()
        .unwrap();
    st.set_explain(2).unwrap();
    let r = st.execute(&mut h).unwrap().unwrap();
    assert!(dump(&r.fetch_all(&mut h, SQLITE3_ASSOC).unwrap())
        .contains("string(17) \"SCAN test_explain\""));
    // freeing the result resets the statement (required to change modes)
    drop(r);
    for bad in [-1, 256] {
        assert_eq!(
            err_text(&st.set_explain(bad).unwrap_err()),
            "ValueError: SQLite3Stmt::setExplain(): Argument #1 ($mode) must be one of the SQLite3Stmt::EXPLAIN_MODE_* constants"
        );
    }
    st.set_explain(0).unwrap();
    assert_eq!(st.explain().unwrap(), 0);
}

#[test]
fn busy_timeout_and_unused_query() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    assert!(db.busy_timeout(0).unwrap());
    assert!(db.busy_timeout(-1000).unwrap());
    assert!(db.busy_timeout(1000).unwrap());
    // unused query() runs through sqlite3_exec: multiple statements execute
    assert!(db
        .query(&mut h, b"CREATE TABLE a (x); CREATE TABLE b (y)", false)
        .unwrap()
        .is_none());
    assert_eq!(
        dump(
            &db.query_single(&mut h, b"SELECT count(*) FROM sqlite_master", false, true)
                .unwrap()
        ),
        "int(2)\n"
    );
    // stmt close twice is fine; operations after close throw
    let st = db.prepare(&mut h, b"SELECT 1").unwrap().unwrap();
    assert!(st.close().unwrap());
    assert_eq!(
        err_text(&st.close().unwrap_err()),
        "Error: The SQLite3 object has not been correctly initialised or is already closed"
    );
}

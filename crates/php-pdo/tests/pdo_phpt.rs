//! Hand ports of `ext/pdo/tests/pdo_0*.phpt` over `sqlite::memory:`.

mod common;

use common::*;
use php_pdo::consts::*;
use php_pdo::ParamId;
use php_runtime::{PhpRef, Value, ZArray};

fn setup(h: &mut TestHost, db: &Value, table: &str, cols: &str, rows: &[&str]) {
    h.exec(db, &format!("CREATE TABLE {table}({cols})"))
        .unwrap();
    for r in rows {
        h.exec(db, &format!("INSERT INTO {table} VALUES({r})"))
            .unwrap();
    }
}

fn three_rows(h: &mut TestHost, db: &Value, table: &str) {
    setup(
        h,
        db,
        table,
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10)",
        &["1, 'A'", "2, 'B'", "3, 'C'"],
    );
}

fn fetch_all_dump(h: &mut TestHost, st: &Value, mode: i64) {
    let r = h.fetch_all(st, Some(mode), &[]).unwrap();
    h.dump(&r);
}

#[test]
fn pdo_001_to_004_basic_modes() {
    for (file, mode) in [
        ("pdo_001.phpt", FETCH_ASSOC),
        ("pdo_002.phpt", FETCH_NUM),
        ("pdo_003.phpt", FETCH_BOTH),
        ("pdo_004.phpt", FETCH_OBJ),
    ] {
        let mut h = TestHost::new();
        let db = h.factory();
        three_rows(&mut h, &db, "t");
        let st = h.prepare(&db, "SELECT * from t").unwrap();
        h.execute(&st, None).unwrap();
        fetch_all_dump(&mut h, &st, mode);
        assert_phpt!(h, file);
    }
}

#[test]
fn pdo_006_group() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "t",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10)",
        &["1, 'A'", "2, 'A'", "3, 'C'"],
    );
    let st = h.prepare(&db, "SELECT val, id from t").unwrap();
    h.execute(&st, None).unwrap();
    fetch_all_dump(&mut h, &st, FETCH_NUM | FETCH_GROUP);
    h.execute(&st, None).unwrap();
    fetch_all_dump(&mut h, &st, FETCH_ASSOC | FETCH_GROUP);
    assert_phpt!(h, "pdo_006.phpt");
}

#[test]
fn pdo_007_008_unique() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "t",
        "id CHAR(1) NOT NULL PRIMARY KEY, val VARCHAR(10)",
        &["'A', 'A'", "'B', 'A'", "'C', 'C'"],
    );
    let st = h.prepare(&db, "SELECT id, val from t").unwrap();
    h.execute(&st, None).unwrap();
    fetch_all_dump(&mut h, &st, FETCH_NUM | FETCH_UNIQUE);
    h.execute(&st, None).unwrap();
    fetch_all_dump(&mut h, &st, FETCH_ASSOC | FETCH_UNIQUE);
    assert_phpt!(h, "pdo_007.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "t",
        "id CHAR(1) NOT NULL PRIMARY KEY, val VARCHAR(10)",
        &["'A', 'A'", "'B', 'A'", "'C', 'C'"],
    );
    let st = h.prepare(&db, "SELECT val, id from t").unwrap();
    h.execute(&st, None).unwrap();
    fetch_all_dump(&mut h, &st, FETCH_NUM | FETCH_UNIQUE);
    assert_phpt!(h, "pdo_008.phpt");
}

fn echo_ctor_class(name: &'static str, props: &[&'static str]) -> Class {
    let mut c = Class::new(name);
    for p in props {
        c = c.prop(p);
    }
    c.ctor(move |h, _, _| {
        h.echo(&format!("{name}::__construct()\n"));
        Ok(())
    })
}

#[test]
fn pdo_010_classtype() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "classtypes010",
        "id int NOT NULL PRIMARY KEY, name VARCHAR(10) NOT NULL UNIQUE",
        &["0, 'stdClass'", "1, 'Test1'", "2, 'Test2'"],
    );
    setup(
        &mut h,
        &db,
        "test010",
        "id int NOT NULL PRIMARY KEY, classtype int, val VARCHAR(10), grp VARCHAR(10)",
        &[
            "1, 0, 'A', 'Group1'",
            "2, 1, 'B', 'Group1'",
            "3, 2, 'C', 'Group2'",
            "4, 3, 'D', 'Group2'",
        ],
    );
    let st = h.prepare(&db, "SELECT classtypes010.name, test010.grp AS grp, test010.id AS id, test010.val AS val FROM test010 LEFT JOIN classtypes010 ON test010.classtype=classtypes010.id").unwrap();
    for c in ["Test1", "Test2", "Test3"] {
        h.add_class(echo_ctor_class(c, &["id", "val"]));
    }
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_CLASS | FETCH_CLASSTYPE | FETCH_GROUP),
            &[s("Test3")],
        )
        .unwrap();
    h.dump(&r);
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_CLASS | FETCH_CLASSTYPE | FETCH_UNIQUE),
            &[s("Test3")],
        )
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_010.phpt");
}

fn test_class_with_name_ctor() -> Class {
    Class::new("TestClass")
        .prop("val")
        .prop("grp")
        .ctor(|h, _, args| {
            let name = args
                .and_then(|a| a.get_int(0).cloned())
                .and_then(|v| v.as_str().map(|s| s.to_string_lossy()))
                .unwrap_or_else(|| "N/A".into());
            h.echo(&format!("TestClass::__construct({name})\n"));
            Ok(())
        })
}

#[test]
fn pdo_012_query_modes() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "test012",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), grp VARCHAR(10)",
        &["1, 'A', 'Group1'", "2, 'B', 'Group2'"],
    );
    let sel = "SELECT val, grp FROM test012";
    let st = h.query_mode(&db, sel, Some(FETCH_NUM), &[]).unwrap();
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    h.add_class(test_class_with_name_ctor());
    let st = h
        .query_mode(&db, sel, Some(FETCH_CLASS), &[s("TestClass")])
        .unwrap();
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    let st = h.query_mode(&db, sel, Some(FETCH_NUM), &[]).unwrap();
    h.set_fetch_mode(
        &st,
        FETCH_CLASS,
        &[s("TestClass"), Value::Array(arr(vec![s("Changed")]))],
    )
    .unwrap();
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_012.phpt");
}

#[test]
fn pdo_013_iteration() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "test013",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), grp VARCHAR(10)",
        &["1, 'A', 'Group1'", "2, 'B', 'Group2'"],
    );
    let sel = "SELECT val, grp FROM test013";
    let st = h.prepare(&db, sel).unwrap();
    h.execute(&st, None).unwrap();
    h.set_fetch_mode(&st, FETCH_NUM, &[]).unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    h.add_class(test_class_with_name_ctor());
    let st = h
        .query_mode(&db, sel, Some(FETCH_CLASS), &[s("TestClass")])
        .unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    let st = h
        .query_mode(
            &db,
            sel,
            Some(FETCH_CLASS),
            &[s("TestClass"), Value::Array(arr(vec![s("WOW")]))],
        )
        .unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    assert_phpt!(h, "pdo_013.phpt");
}

#[test]
fn pdo_015_fetch_column() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "test015",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(20)",
        &["1, 'A', 'A2'", "2, 'A', 'B2'"],
    );
    let st = h.prepare(&db, "SELECT id, val, val2 FROM test015").unwrap();
    let cases: Vec<(&str, i64, Vec<Value>)> = vec![
        ("PDO::FETCH_COLUMN", FETCH_COLUMN, vec![]),
        ("PDO::FETCH_COLUMN, 2", FETCH_COLUMN, vec![i(2)]),
        (
            "PDO::FETCH_COLUMN|PDO::FETCH_GROUP",
            FETCH_COLUMN | FETCH_GROUP,
            vec![],
        ),
        (
            "PDO::FETCH_COLUMN|PDO::FETCH_UNIQUE",
            FETCH_COLUMN | FETCH_UNIQUE,
            vec![],
        ),
        (
            "PDO::FETCH_COLUMN|PDO::FETCH_UNIQUE, 0",
            FETCH_COLUMN | FETCH_UNIQUE,
            vec![i(0)],
        ),
        (
            "PDO::FETCH_COLUMN|PDO::FETCH_UNIQUE, 1",
            FETCH_COLUMN | FETCH_UNIQUE,
            vec![i(1)],
        ),
        (
            "PDO::FETCH_COLUMN|PDO::FETCH_UNIQUE, 2",
            FETCH_COLUMN | FETCH_UNIQUE,
            vec![i(2)],
        ),
    ];
    for (label, mode, args) in cases {
        h.execute(&st, None).unwrap();
        h.echo(&format!("$selectIdVal1Val2->fetchAll({label})\n"));
        let r = h.fetch_all(&st, Some(mode), &args).unwrap();
        h.dump(&r);
    }
    let st = h.prepare(&db, "SELECT val, val2 FROM test015").unwrap();
    h.execute(&st, None).unwrap();
    h.echo("$selectVal1Val2->fetchAll(PDO::FETCH_COLUMN|PDO::FETCH_GROUP)\n");
    let r = h
        .fetch_all(&st, Some(FETCH_COLUMN | FETCH_GROUP), &[])
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_015.phpt");
}

fn pdo_016_body(h: &mut TestHost, colon: bool, file: &str) {
    let db = h.factory();
    setup(
        h,
        &db,
        "t016",
        "idx int NOT NULL PRIMARY KEY, txt VARCHAR(20)",
        &["0, 'String0'", "1, 'String1'", "2, 'String2'"],
    );
    let st1 = h.prepare(&db, "SELECT COUNT(idx) FROM t016").unwrap();
    let st2 = h
        .prepare(&db, "SELECT idx, txt FROM t016 ORDER by idx")
        .unwrap();
    h.execute(&st1, None).unwrap();
    let v = h.fetch_column(&st1, 0).unwrap();
    h.dump(&v);
    drop(st1);
    h.execute(&st2, None).unwrap();
    let cont = h
        .fetch_all(&st2, Some(FETCH_COLUMN | FETCH_UNIQUE), &[])
        .unwrap();
    h.dump(&cont);
    h.echo("===WHILE===\n");
    let idx = PhpRef::new(Value::Null);
    let txt = PhpRef::new(Value::Null);
    h.bind_column(&st2, ParamId::Name(z("idx")), &idx, PARAM_STR)
        .unwrap();
    h.bind_column(&st2, ParamId::Name(z("txt")), &txt, PARAM_STR)
        .unwrap();
    h.execute(&st2, None).unwrap();
    let pair = |idx: &PhpRef, txt: &PhpRef| {
        let k = idx.get();
        let mut a = ZArray::new();
        let key = match k {
            Value::Int(n) => php_runtime::ArrayKey::Int(n),
            Value::Str(s) => php_runtime::ArrayKey::from_zstr(&s),
            _ => php_runtime::ArrayKey::from_bytes(b""),
        };
        a.make_mut().insert(key, txt.get());
        Value::Array(a)
    };
    while matches!(h.fetch(&st2, FETCH_BOUND).unwrap(), Value::Bool(true)) {
        let p = pair(&idx, &txt);
        h.dump(&p);
    }
    h.echo("===ALONE===\n");
    let st3 = h
        .prepare(&db, "SELECT txt FROM t016 WHERE idx=:inp")
        .unwrap();
    let n = |s: &str| {
        z(&if colon {
            format!(":{s}")
        } else {
            s.to_string()
        })
    };
    h.bind_param(&st3, ParamId::Name(n("inp")), &idx, PARAM_STR)
        .unwrap();
    let st4 = h
        .prepare(&db, "SELECT idx FROM t016 WHERE txt=:txt")
        .unwrap();
    h.bind_param(&st4, ParamId::Name(n("txt")), &txt, PARAM_STR)
        .unwrap();
    let col1 = PhpRef::new(Value::Null);
    let col2 = PhpRef::new(Value::Null);
    let Value::Array(cont) = cont else { panic!() };
    for (n, (k, v)) in cont.iter().enumerate() {
        idx.set(k.to_value());
        txt.set(v.clone());
        let p = pair(&idx, &txt);
        h.dump(&p);
        let r = Value::Bool(h.execute(&st3, None).unwrap());
        h.dump(&r);
        if n == 0 {
            h.bind_column(&st3, ParamId::Name(z("txt")), &col1, PARAM_STR)
                .unwrap();
        }
        let r = h.fetch(&st3, FETCH_BOUND).unwrap();
        h.dump(&r);
        h.close_cursor(&st3).unwrap();
        let r = Value::Bool(h.execute(&st4, None).unwrap());
        h.dump(&r);
        if n == 0 {
            h.bind_column(&st4, ParamId::Name(z("idx")), &col2, PARAM_STR)
                .unwrap();
        }
        let r = h.fetch(&st4, FETCH_BOUND).unwrap();
        h.dump(&r);
        h.close_cursor(&st4).unwrap();
        let p = pair(&col2, &col1);
        h.dump(&p);
    }
    h.echo("===REBIND/SAME===\n");
    h.bind_column(&st4, ParamId::Name(z("idx")), &col1, PARAM_STR)
        .unwrap();
    for (k, v) in cont.iter() {
        idx.set(k.to_value());
        txt.set(v.clone());
        let p = pair(&idx, &txt);
        h.dump(&p);
        let r = Value::Bool(h.execute(&st3, None).unwrap());
        h.dump(&r);
        let r = h.fetch(&st3, FETCH_BOUND).unwrap();
        h.dump(&r);
        h.close_cursor(&st3).unwrap();
        h.dump(&col1.get());
        let r = Value::Bool(h.execute(&st4, None).unwrap());
        h.dump(&r);
        let r = h.fetch(&st4, FETCH_BOUND).unwrap();
        h.dump(&r);
        h.close_cursor(&st4).unwrap();
        h.dump(&col1.get());
    }
    h.echo("===REBIND/CONFLICT===\n");
    h.bind_column(&st2, ParamId::Name(z("idx")), &col1, PARAM_STR)
        .unwrap();
    h.bind_column(&st2, ParamId::Name(z("txt")), &col1, PARAM_STR)
        .unwrap();
    h.execute(&st2, None).unwrap();
    while matches!(h.fetch(&st2, FETCH_BOUND).unwrap(), Value::Bool(true)) {
        h.dump(&col1.get());
    }
    assert_phpt!(h, file);
}

#[test]
fn pdo_016_bound_columns() {
    let mut h = TestHost::new();
    pdo_016_body(&mut h, true, "pdo_016.phpt");
    let mut h = TestHost::new();
    pdo_016_body(&mut h, false, "pdo_016a.phpt");
}

#[test]
fn pdo_017_transactions() {
    let mut h = TestHost::new();
    let db = h.factory();
    three_rows(&mut h, &db, "test017");
    let delete = h.prepare(&db, "DELETE FROM test017").unwrap();
    let count_rows = |h: &mut TestHost, action: &str| {
        let sel = h.prepare(&db, "SELECT COUNT(*) FROM test017").unwrap();
        h.execute(&sel, None).unwrap();
        let res = h.fetch_column(&sel, 0).unwrap();
        let res = res.as_str().unwrap().to_string_lossy();
        h.echo(&format!("Counted {res} rows after {action}.\n"));
    };
    let pdo = TestHost::pdo(&db);
    count_rows(&mut h, "insert");
    pdo.begin_transaction(&mut h).unwrap();
    h.execute(&delete, None).unwrap();
    count_rows(&mut h, "delete");
    pdo.roll_back(&mut h).unwrap();
    count_rows(&mut h, "rollback");
    pdo.begin_transaction(&mut h).unwrap();
    h.execute(&delete, None).unwrap();
    count_rows(&mut h, "delete");
    pdo.commit(&mut h).unwrap();
    count_rows(&mut h, "commit");
    assert_phpt!(h, "pdo_017.phpt");
}

#[test]
fn pdo_019_fetch_bound() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "test019",
        "idx int NOT NULL PRIMARY KEY, txt VARCHAR(20)",
        &[
            "0, 'String0'",
            "1, 'String1'",
            "2, 'String2'",
            "3, 'String3'",
        ],
    );
    let q = h.query(&db, "SELECT COUNT(*) FROM test019").unwrap();
    let v = h.fetch_column(&q, 0).unwrap();
    h.dump(&v);
    let st = h
        .prepare(&db, "SELECT idx, txt FROM test019 ORDER by idx")
        .unwrap();
    h.execute(&st, None).unwrap();
    let cont = h
        .fetch_all(&st, Some(FETCH_COLUMN | FETCH_UNIQUE), &[])
        .unwrap();
    h.dump(&cont);
    h.echo("===WHILE===\n");
    let idx = PhpRef::new(Value::Null);
    let txt = PhpRef::new(Value::Null);
    h.bind_column(&st, ParamId::Name(z("idx")), &idx, PARAM_STR)
        .unwrap();
    h.bind_column(&st, ParamId::Name(z("txt")), &txt, PARAM_STR)
        .unwrap();
    h.execute(&st, None).unwrap();
    while matches!(h.fetch(&st, FETCH_BOUND).unwrap(), Value::Bool(true)) {
        let k = php_runtime::ArrayKey::from_zstr(idx.get().as_str().unwrap());
        let a = ZArray::from_pairs([(k, txt.get())]);
        h.dump(&Value::Array(a));
    }
    assert_phpt!(h, "pdo_019.phpt");
}

#[test]
fn pdo_020_column_count() {
    let mut h = TestHost::new();
    let db = h.factory();
    setup(
        &mut h,
        &db,
        "test020",
        "id INT NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(16)",
        &["1, 'A', 'A'", "2, 'B', 'B'", "3, 'C', 'C'"],
    );
    for sql in [
        "SELECT id, val FROM test020",
        "SELECT id, val, val2 FROM test020",
        "SELECT COUNT(*) FROM test020",
    ] {
        let st = h.query(&db, sql).unwrap();
        let n = TestHost::stmt(&st).column_count(&mut h).unwrap();
        h.echo(&format!("Counted {n} columns after {sql}.\n"));
    }
    assert_phpt!(h, "pdo_020.phpt");
}

fn data_rows() -> Vec<[&'static str; 3]> {
    vec![
        ["10", "Abc", "zxy"],
        ["20", "Def", "wvu"],
        ["30", "Ghi", "tsr"],
        ["40", "Jkl", "qpo"],
        ["50", "Mno", "nml"],
        ["60", "Pqr", "kji"],
    ]
}

#[test]
fn pdo_021_placeholders() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test021(id INT NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(16))",
    )
    .unwrap();
    let select = h.prepare(&db, "SELECT COUNT(id) FROM test021").unwrap();
    let st = h
        .prepare(&db, "INSERT INTO test021 VALUES(?, ?, ?)")
        .unwrap();
    for row in data_rows() {
        h.execute(&st, Some(arr(row.iter().map(|v| s(v)).collect())))
            .unwrap();
    }
    h.execute(&select, None).unwrap();
    let n = h.fetch_column(&select, 0).unwrap();
    let n = n.as_str().unwrap().to_string_lossy();
    h.echo(&format!("There are {n} rows in the table.\n"));
    let st2 = h
        .prepare(&db, "INSERT INTO test021 VALUES(:first, :second, :third)")
        .unwrap();
    for row in data_rows() {
        let first: i64 = row[0].parse::<i64>().unwrap() + 5;
        h.execute(
            &st2,
            Some(assoc(vec![
                (":first", i(first)),
                (":second", s(row[1])),
                (":third", s(row[2])),
            ])),
        )
        .unwrap();
    }
    h.execute(&select, None).unwrap();
    let n = h.fetch_column(&select, 0).unwrap();
    let n = n.as_str().unwrap().to_string_lossy();
    h.echo(&format!("There are {n} rows in the table.\n"));
    assert_phpt!(h, "pdo_021.phpt");
}

#[test]
fn pdo_024_bind_null() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "create table test024 (id int, name varchar(10))")
        .unwrap();
    let st = h
        .prepare(&db, "insert into test024 (id, name) values(0, :name)")
        .unwrap();
    let name = PhpRef::new(Value::Null);
    h.bind_param(&st, ParamId::Name(z(":name")), &name, PARAM_NULL)
        .unwrap();
    h.echo(if name.get().is_null() {
        "bind: success\n"
    } else {
        "bind: fail\n"
    });
    let r = Value::Bool(h.execute(&st, None).unwrap());
    h.dump(&r);
    let q = h.query(&db, "select name from test024 where id=0").unwrap();
    let v = h.fetch_column(&q, 0).unwrap();
    h.dump(&v);
    assert_phpt!(h, "pdo_024.phpt");
}

#[test]
fn pdo_025_fetch_into() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test025(id INT NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(16))",
    )
    .unwrap();
    let st = h
        .prepare(&db, "INSERT INTO test025 VALUES(?, ?, ?)")
        .unwrap();
    for row in data_rows() {
        h.execute(&st, Some(arr(row.iter().map(|v| s(v)).collect())))
            .unwrap();
    }
    h.add_class(Class::new("Test").prop("id").prop("val").prop("val2"));
    let st = h.prepare(&db, "SELECT * FROM test025").unwrap();
    let o = h.new_object("Test");
    h.set_fetch_mode(&st, FETCH_INTO, &[o]).unwrap();
    h.execute(&st, None).unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    h.echo("===FAIL===\n");
    h.add_class(
        Class::new("Fail")
            .prop_vis("id", Vis::Protected)
            .prop("val")
            .prop("val2"),
    );
    let o = h.new_object("Fail");
    h.set_fetch_mode(&st, FETCH_INTO, &[o]).unwrap();
    h.execute(&st, None).unwrap();
    let r = h.foreach(&st, |h, _, v| h.dump(&v));
    let e = r.unwrap_err();
    h.uncaught_in(&e, "pdo_025.php");
    assert_phpt!(h, "pdo_025.phpt");
}

#[test]
fn pdo_027_lazy_query() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "create table test027 (id int, name varchar(10))")
        .unwrap();
    h.exec(&db, "INSERT INTO test027 (id,name) VALUES(1,'test1')")
        .unwrap();
    h.exec(&db, "INSERT INTO test027 (id,name) VALUES(2,'test2')")
        .unwrap();
    let mut alias: Option<Value> = None;
    let r = h
        .query_mode(&db, "SELECT * FROM test027", Some(FETCH_LAZY), &[])
        .unwrap();
    h.dump(&r);
    h.foreach(&r, |h, _, v| {
        let same = match (&alias, &v) {
            (Some(Value::Object(a)), Value::Object(b)) => php_runtime::Obj::ptr_eq(a, b),
            _ => false,
        };
        h.dump(&Value::Bool(same));
        h.dump(&v);
        alias = Some(v.clone());
        let row = TestHost::row(&v).unwrap();
        let id = row.read_property(h, &z("id")).unwrap();
        let name = row.read_property(h, &z("name")).unwrap();
        h.echo(&format!(
            "lazy: {}{}\n",
            id.as_str().unwrap().to_string_lossy(),
            name.as_str().unwrap().to_string_lossy()
        ));
    })
    .unwrap();
    h.echo("End\n");
    assert_phpt!(h, "pdo_027.phpt");
}

#[test]
fn pdo_028_bind_value_positions() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test028(id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10), val3 VARCHAR(10))").unwrap();
    let st = h
        .prepare(&db, "INSERT INTO test028 values (1, ?, ?, ?)")
        .unwrap();
    for (n, v) in ["one", "two", "three"].iter().enumerate() {
        h.bind_value(&st, ParamId::Position(n as i64 + 1), s(v), PARAM_STR)
            .unwrap();
    }
    h.execute(&st, None).unwrap();
    let st = h.prepare(&db, "SELECT * from test028").unwrap();
    h.execute(&st, None).unwrap();
    fetch_all_dump(&mut h, &st, FETCH_ASSOC);
    assert_phpt!(h, "pdo_028.phpt");
}

#[test]
fn pdo_032_case() {
    let mut h = TestHost::new();
    let db = h.factory();
    three_rows(&mut h, &db, "test032");
    for case in [CASE_LOWER, CASE_UPPER] {
        h.set_attr(&db, ATTR_CASE, i(case)).unwrap();
        let st = h.prepare(&db, "SELECT * from test032").unwrap();
        h.execute(&st, None).unwrap();
        fetch_all_dump(&mut h, &st, FETCH_ASSOC);
        h.close_cursor(&st).unwrap();
    }
    assert_phpt!(h, "pdo_032.phpt");
}

#[test]
fn pdo_033_quote() {
    let mut h = TestHost::new();
    let db = h.factory();
    let unquoted = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
    let quoted = h.quote(&db, unquoted, PARAM_STR).unwrap();
    let quoted = quoted.as_str().unwrap().to_string_lossy();
    let len = unquoted.len();
    h.query(&db, &format!("CREATE TABLE test033 (t char({len}))"))
        .unwrap();
    h.query(&db, &format!("INSERT INTO test033 (t) VALUES({quoted})"))
        .unwrap();
    let st = h.prepare(&db, "SELECT * from test033").unwrap();
    h.execute(&st, None).unwrap();
    let r = h.fetch_all(&st, Some(FETCH_ASSOC), &[]).unwrap();
    h.print_r(&r);
    assert_phpt!(h, "pdo_033.phpt");
}

#[test]
fn pdo_034_key_pair() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test034 (a varchar(100), b varchar(100), c varchar(100))",
    )
    .unwrap();
    for n in 0..5 {
        h.exec(
            &db,
            &format!("INSERT INTO test034 (a,b,c) VALUES('test{n}','{n}','{n}')"),
        )
        .unwrap();
    }
    let q = h.query(&db, "SELECT a,b FROM test034").unwrap();
    let v = h.fetch(&q, FETCH_KEY_PAIR).unwrap();
    h.dump(&v);
    let q = h.query(&db, "SELECT a,b FROM test034").unwrap();
    let v = h.fetch_all(&q, Some(FETCH_KEY_PAIR), &[]).unwrap();
    h.dump(&v);
    let q = h.query(&db, "SELECT * FROM test034").unwrap();
    let v = h.fetch(&q, FETCH_KEY_PAIR).unwrap();
    h.dump(&v);
    let q = h.query(&db, "SELECT a,a FROM test034").unwrap();
    let v = h.fetch_all(&q, Some(FETCH_KEY_PAIR), &[]).unwrap();
    h.dump(&v);
    assert_phpt!(h, "pdo_034.phpt");
}

#[test]
fn pdo_035_pdorow() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test_pdo_35_pdo_row (id int, name varchar(10))",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO test_pdo_35_pdo_row VALUES (23, '0')")
        .unwrap();
    let st = h
        .prepare(&db, "SELECT id, name FROM test_pdo_35_pdo_row")
        .unwrap();
    h.execute(&st, None).unwrap();
    let result = h.fetch(&st, FETCH_LAZY).unwrap();
    h.dump(&result);
    h.dump(&Value::Bool(false));
    let row = TestHost::row(&result).unwrap();
    for offset in [i(0), s("0"), s("id"), s("name"), i(1)] {
        let exported = h.export(&offset);
        h.echo(&format!("Offset: {exported}\n"));
        h.echo("Dimension:\n");
        h.echo("Isset:\n");
        for _ in 0..2 {
            let b = row.has_dimension(&mut h, &offset, false).unwrap();
            h.dump(&Value::Bool(b));
        }
        h.echo("Empty:\n");
        for _ in 0..2 {
            let b = !row.has_dimension(&mut h, &offset, true).unwrap();
            h.dump(&Value::Bool(b));
        }
        h.echo("Null coalesce:\n");
        for _ in 0..2 {
            let v = if row.has_dimension(&mut h, &offset, false).unwrap() {
                row.read_dimension(&mut h, Some(&offset)).unwrap()
            } else {
                s("default")
            };
            let v = if v.is_null() { s("default") } else { v };
            h.dump(&v);
        }
        h.echo("Read:\n");
        for _ in 0..2 {
            let v = row.read_dimension(&mut h, Some(&offset)).unwrap();
            h.dump(&v);
        }
        let name = match &offset {
            Value::Int(n) => z(&n.to_string()),
            Value::Str(s) => s.clone(),
            _ => unreachable!(),
        };
        h.echo("Property:\n");
        h.echo("Isset:\n");
        for _ in 0..2 {
            let b = row.has_property(&mut h, &name, false).unwrap();
            h.dump(&Value::Bool(b));
        }
        h.echo("Empty:\n");
        for _ in 0..2 {
            let b = !row.has_property(&mut h, &name, true).unwrap();
            h.dump(&Value::Bool(b));
        }
        h.echo("Null coalesce:\n");
        for _ in 0..2 {
            let v = if row.has_property(&mut h, &name, false).unwrap() {
                row.read_property(&mut h, &name).unwrap()
            } else {
                s("default")
            };
            h.dump(&v);
        }
        h.echo("Read:\n");
        for _ in 0..2 {
            let v = row.read_property(&mut h, &name).unwrap();
            h.dump(&v);
        }
    }
    h.echo("Errors:\n");
    let errs = [
        php_pdo::PdoRow::write_dimension_error(false),
        php_pdo::PdoRow::write_dimension_error(true),
    ];
    for e in &errs {
        h.echo(&format!("{}\n", e.message().unwrap()));
    }
    h.echo("\nNotice: Indirect modification of overloaded element of PDORow has no effect in /t on line 1\n");
    let e = row.read_dimension(&mut h, None).unwrap_err();
    h.echo(&format!("{}\n", e.message().unwrap()));
    for e in [
        php_pdo::PdoRow::unset_dimension_error(),
        php_pdo::PdoRow::write_property_error(),
        php_pdo::PdoRow::unset_property_error(),
    ] {
        h.echo(&format!("{}\n", e.message().unwrap()));
    }
    let out = h.output().replacen("bool(false)\n", "bool(false)\n", 1);
    // `get_parent_class($result)` is a VM concern: it prints bool(false).
    assert!(check_phpt("pdo", "pdo_035.phpt", &out));
}

#[test]
fn pdo_038_fetch_column_index() {
    let mut h = TestHost::new();
    let db = h.factory();
    let st = h.prepare(&db, "SELECT 1").unwrap();
    for idx in [-1, 0, 1] {
        h.execute(&st, None).unwrap();
        match h.fetch_column(&st, idx) {
            Ok(v) => h.dump(&v),
            Err(e) => h.echo(&format!("{}\n", e.message().unwrap())),
        }
    }
    assert_phpt!(h, "pdo_038.phpt");
}

#[test]
fn pdo_039_error_code() {
    let mut h = TestHost::new();
    let conn = h.new_pdo("PDO", "sqlite::memory:", None).unwrap();
    let v = h.db_error_code(&conn).unwrap();
    h.dump(&v);
    h.get_attr(&conn, ATTR_DRIVER_NAME).unwrap();
    let v = h.db_error_code(&conn).unwrap();
    h.dump(&v);
    let v = h.db_error_code(&conn).unwrap();
    h.dump(&v);
    let st = h.prepare(&conn, "SELECT 1").unwrap();
    let v = h.db_error_code(&conn).unwrap();
    h.dump(&v);
    let v = TestHost::stmt(&st).error_code(&mut h).unwrap();
    h.dump(&v);
    h.execute(&st, None).unwrap();
    let v = TestHost::stmt(&st).error_code(&mut h).unwrap();
    h.dump(&v);
    let v = TestHost::stmt(&st).error_code(&mut h).unwrap();
    h.dump(&v);
    assert_phpt!(h, "pdo_039.phpt");
}

#[test]
fn harness_rejects_wrong_output() {
    assert!(!check_phpt("pdo", "pdo_001.phpt", "garbage"));
    assert!(!check_phpt("pdo", "pdo_034.phpt", ""));
    assert!(expectf_match("a %s b", "a xyz b"));
    assert!(!expectf_match("a %d b", "a x b"));
    assert!(expectf_match("x%Ay", "x\n\ny"));
}

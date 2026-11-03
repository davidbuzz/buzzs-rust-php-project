//! Hand ports of the fetch-mode `ext/pdo/tests/*.phpt` over
//! `sqlite::memory:` (and `sqlemu:`, SQLite behind PDO's prepare
//! emulation).

mod common;

use common::*;
use php_pdo::consts::*;
use php_pdo::ParamId;
use php_runtime::{RuntimeError, Value, ZArray};

fn rows(h: &mut TestHost, db: &Value, table: &str, cols: &str, rows: &[&str]) {
    h.exec(db, &format!("CREATE TABLE {table}({cols})"))
        .unwrap();
    for r in rows {
        h.exec(db, &format!("INSERT INTO {table} VALUES({r})"))
            .unwrap();
    }
}

fn four(h: &mut TestHost, db: &Value, table: &str) {
    rows(
        h,
        db,
        table,
        "id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10)",
        &[
            "1, 'A', 'alpha'",
            "2, 'B', 'beta'",
            "3, 'C', 'gamma'",
            "4, 'D', 'delta'",
        ],
    );
}

fn print_err(h: &mut TestHost, e: &RuntimeError) {
    let d = h.describe_error(e);
    h.echo(&format!("{d}\n"));
}

fn sv(v: &Value) -> String {
    v.as_str().map(|s| s.to_string_lossy()).unwrap_or_default()
}

#[test]
fn fetch_class_basic() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "test005",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10)",
        &["1, 'A', 'AA'", "2, 'B', 'BB'", "3, 'C', 'CC'"],
    );
    let st = h.prepare(&db, "SELECT id, val, val2 from test005").unwrap();
    h.add_class(
        Class::new("TestBase")
            .prop("id")
            .prop_vis("val", Vis::Protected)
            .prop_vis("val2", Vis::Private),
    );
    h.add_class(
        Class::new("TestDerived")
            .extends("TestBase")
            .dynamic()
            .prop_vis("row", Vis::Protected)
            .ctor(|h, this, args| {
                let r = args.unwrap().get_int(0).unwrap().clone();
                let Value::Reference(r) = r else {
                    panic!("by ref")
                };
                let n = r.get().as_int().unwrap();
                let id = sv(&TestHost::get_prop(this, "id").unwrap());
                h.echo(&format!("TestDerived::__construct({n},{id})\n"));
                TestHost::set_prop(this, "row", Value::Int(n));
                r.set(Value::Int(n + 1));
                Ok(())
            }),
    );
    h.execute(&st, None).unwrap();
    let r = h.fetch_all(&st, Some(FETCH_CLASS), &[]).unwrap();
    h.dump(&r);
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(&st, Some(FETCH_CLASS), &[s("TestBase")])
        .unwrap();
    h.dump(&r);
    h.execute(&st, None).unwrap();
    let counter = php_runtime::PhpRef::new(i(0));
    let args = arr(vec![Value::Reference(counter.clone())]);
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_CLASS),
            &[s("TestDerived"), Value::Array(args)],
        )
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_class_basic.phpt");
}

#[test]
fn fetch_class_private_constructor() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "t",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10)",
        &["1, 'A', 'AA'", "2, 'B', 'BB'"],
    );
    let st = h.prepare(&db, "SELECT id, val FROM t").unwrap();
    h.add_class(
        Class::new("TestPrivateCtor")
            .prop("id")
            .prop("val")
            .private_ctor()
            .ctor(|h, this, args| {
                let a = sv(args.unwrap().get_int(0).unwrap());
                let id = sv(&TestHost::get_prop(this, "id").unwrap());
                h.echo(&format!("TestPrivateCtor::__construct({a}, {id})\n"));
                Ok(())
            }),
    );
    h.add_class(Class::new("TestDerivedPrivateCtor").extends("TestPrivateCtor"));
    for (c, a) in [
        ("TestPrivateCtor", "test"),
        ("TestDerivedPrivateCtor", "testFromDerived"),
    ] {
        h.execute(&st, None).unwrap();
        let r = h
            .fetch_all(
                &st,
                Some(FETCH_CLASS),
                &[s(c), Value::Array(arr(vec![s(a)]))],
            )
            .unwrap();
        h.dump(&r);
    }
    assert_phpt!(h, "pdo_fetch_class_private_constructor.phpt");
}

#[test]
fn fetch_class_with_classtype_flag() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "classtypes009",
        "id int NOT NULL PRIMARY KEY, name VARCHAR(10) NOT NULL UNIQUE",
        &["0, 'stdClass'", "1, 'Test1'", "2, 'Test2'"],
    );
    rows(
        &mut h,
        &db,
        "test009",
        "id int NOT NULL PRIMARY KEY, classtype int, val VARCHAR(10)",
        &["1, 0, 'A'", "2, 1, 'B'", "3, 2, 'C'", "4, 3, 'D'"],
    );
    let st = h.prepare(&db, "SELECT classtypes009.name, test009.id AS id, test009.val AS val FROM test009 LEFT JOIN classtypes009 ON test009.classtype=classtypes009.id").unwrap();
    for c in ["Test1", "Test2", "Test3"] {
        h.add_class(Class::new(c).prop("id").prop("val").ctor(move |h, _, _| {
            h.echo(&format!("{c}::__construct()\n"));
            Ok(())
        }));
    }
    h.execute(&st, None).unwrap();
    let r = h.fetch_all(&st, Some(FETCH_NUM), &[]).unwrap();
    h.dump(&r);
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(&st, Some(FETCH_CLASS | FETCH_CLASSTYPE), &[s("Test3")])
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_class_with_classtype_flag.phpt");
}

fn reentrant_test_class(h: &mut TestHost) {
    h.add_class(
        Class::new("Test")
            .typed("val1", "string")
            .typed("val2", "string")
            .ctor(|h, this, args| {
                let v = args.unwrap().get_int(0).cloned().unwrap_or(Value::Null);
                h.dump(&v);
                if let Value::Object(o) = &v {
                    if h.is_a(&o.class_name().to_string_lossy(), "PDOStatement") {
                        let val2 = TestHost::get_prop(this, "val2").unwrap_or(Value::Null);
                        h.set_fetch_mode(
                            &v,
                            FETCH_CLASS,
                            &[s("Test"), Value::Array(arr(vec![val2]))],
                        )?;
                    }
                }
                Ok(())
            }),
    );
}

#[test]
fn fetch_class_change_ctor_args_during_fetch() {
    for n in 1..=4 {
        let mut h = TestHost::new();
        let db = h.factory();
        four(&mut h, &db, "t");
        reentrant_test_class(&mut h);
        let tables = ["one", "two", "three", "four"];
        let q = format!(
            "SELECT val1, val2 FROM pdo_fetch_class_change_ctor_{}",
            tables[n - 1]
        );
        h.exec(
            &db,
            &format!(
                "ALTER TABLE t RENAME TO pdo_fetch_class_change_ctor_{}",
                tables[n - 1]
            ),
        )
        .unwrap();
        let st = h.prepare(&db, &q).unwrap();
        let args = Value::Array(arr(vec![st.clone()]));
        if n == 1 || n == 3 {
            h.set_fetch_mode(&st, FETCH_CLASS, &[s("Test"), args.clone()])
                .unwrap();
        }
        h.execute(&st, None).unwrap();
        let r = match n {
            1 => h.fetch(&st, FETCH_DEFAULT),
            2 => {
                let a = arr(vec![st.clone()]);
                h.fetch_object(&st, Some("Test"), Some(a))
            }
            3 => h.fetch_all(&st, None, &[]),
            _ => h.fetch_all(&st, Some(FETCH_CLASS), &[s("Test"), args]),
        };
        match r {
            Ok(v) => h.dump(&v),
            Err(e) => print_err(&mut h, &e),
        }
        assert_phpt!(
            h,
            &format!("pdo_fetch_class_change_ctor_args_during_fetch{n}.phpt")
        );
    }
}

#[test]
fn fetch_class_ctor_with_named_arguments() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "t",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10)",
        &["1, 'A', 'AA'", "2, 'B', 'BB'", "3, 'C', 'CC'"],
    );
    h.add_class(
        Class::new("TestBase")
            .prop("id")
            .prop_vis("val", Vis::Protected)
            .prop_vis("val2", Vis::Private)
            .ctor(|h, _, args| {
                let args = args.unwrap();
                let a = sv(args.get_str(b"a").unwrap());
                let b = sv(args.get_str(b"b").unwrap());
                h.echo(&format!("Value of $a: {a}\nValue of $b: {b}\n"));
                Ok(())
            }),
    );
    let st = h.prepare(&db, "SELECT id, val, val2 from t").unwrap();
    h.execute(&st, None).unwrap();
    let args = assoc(vec![("b", s("My key is B")), ("a", s("My key is A"))]);
    let r = h
        .fetch_all(&st, Some(FETCH_CLASS), &[s("TestBase"), Value::Array(args)])
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_class_ctor_with_named_arguments.phpt");
}

fn func_rows(h: &mut TestHost, db: &Value, table: &str) {
    four(h, db, table);
}

#[test]
fn fetch_function_basic() {
    let mut h = TestHost::new();
    let db = h.factory();
    func_rows(&mut h, &db, "pdo_fetch_function_basic");
    h.add_function("associateValWithId", |h, a| {
        let id = sv(&a[0]).parse::<i64>().unwrap();
        let val = sv(&a[1]);
        h.echo(&format!("associateValWithId({id}, '{val}')\n"));
        Ok(Value::Array(iassoc(vec![(id, s(&val))])))
    });
    h.add_function("selectAllCallback", |h, a| {
        let id = sv(&a[0]);
        let (v1, v2) = (sv(&a[1]), sv(&a[2]));
        h.echo(&format!("selectAllCallback({id}, '{v1}', '{v2}')\n"));
        Ok(s(&format!("{v1}{v2}")))
    });
    h.echo("SELECT id, val1:\n");
    let st = h
        .prepare(&db, "SELECT id, val1 FROM pdo_fetch_function_basic")
        .unwrap();
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(&st, Some(FETCH_FUNC), &[s("associateValWithId")])
        .unwrap();
    h.dump(&r);
    h.echo("SELECT *:\n");
    let st = h
        .prepare(&db, "SELECT * FROM pdo_fetch_function_basic")
        .unwrap();
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(&st, Some(FETCH_FUNC), &[s("selectAllCallback")])
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_function_basic.phpt");
}

fn to_upper_with_log(h: &mut TestHost) {
    h.add_function("to_upper_with_log", |h, a| {
        let v = sv(&a[0]);
        h.echo(&format!("to_upper_with_log('{v}')\n"));
        Ok(s(&v.to_ascii_uppercase()))
    });
}

#[test]
fn fetch_function_grouping_and_unique() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "g",
        "id int NOT NULL PRIMARY KEY, val VARCHAR(10), grp VARCHAR(10)",
        &[
            "1, 'A', 'Group1'",
            "2, 'B', 'Group1'",
            "3, 'C', 'Group2'",
            "4, 'D', 'Group2'",
        ],
    );
    h.add_function("associateValWithId", |h, a| {
        let id = sv(&a[0]);
        let v = sv(&a[1]);
        h.echo(&format!("associateValWithId({id}, '{v}')\n"));
        Ok(s(&format!("{id} => {v}")))
    });
    let st = h.prepare(&db, "SELECT grp, id, val FROM g").unwrap();
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_FUNC | FETCH_GROUP),
            &[s("associateValWithId")],
        )
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_function_with_grouping.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    func_rows(&mut h, &db, "u");
    to_upper_with_log(&mut h);
    let st = h.prepare(&db, "SELECT val1, val2 FROM u").unwrap();
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_FUNC | FETCH_UNIQUE),
            &[s("to_upper_with_log")],
        )
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_function_with_unique.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "ug",
        "id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10), grp VARCHAR(10)",
        &[
            "1, 'A', 'alpha', 'Group1'",
            "2, 'B', 'beta', 'Group1'",
            "3, 'C', 'gamma', 'Group2'",
            "4, 'D', 'delta', 'Group2'",
        ],
    );
    to_upper_with_log(&mut h);
    let st = h.prepare(&db, "SELECT grp, val2, val1 FROM ug").unwrap();
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_FUNC | FETCH_UNIQUE | FETCH_GROUP),
            &[s("to_upper_with_log")],
        )
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_function_with_unique_and_grouping.phpt");
}

#[test]
fn fetch_function_exception_in_call() {
    let mut h = TestHost::new();
    let db = h.factory();
    func_rows(&mut h, &db, "e");
    h.add_function("bogusCallback", |h, a| {
        let (v1, v2) = (sv(&a[0]), sv(&a[1]));
        h.echo(&format!("{v1}: {v2}\n"));
        if v2 == "gamma" {
            return Err(RuntimeError::Custom {
                class: z("Exception"),
                message: "GAMMA IS BAD".into(),
            });
        }
        Ok(s(&format!("{v1}: {v2}")))
    });
    let st = h.prepare(&db, "SELECT val1, val2 FROM e").unwrap();
    h.execute(&st, None).unwrap();
    match h.fetch_all(&st, Some(FETCH_FUNC), &[s("bogusCallback")]) {
        Ok(v) => h.dump(&v),
        Err(e) => print_err(&mut h, &e),
    }
    assert_phpt!(h, "pdo_fetch_function_exception_in_call.phpt");
}

#[test]
fn fetch_function_incorrect_callables() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "c",
        "id int NOT NULL PRIMARY KEY, pl_name VARCHAR(10)",
        &["1, 'php'", "2, 'sql'"],
    );
    h.add_class(
        Class::new("Bar")
            .method("privateStatic", true, Vis::Private, |_, _, a| {
                Ok(a[0].clone())
            })
            .method("instanceMethod", false, Vis::Public, |_, _, a| {
                Ok(a[0].clone())
            }),
    );
    let st = h.query(&db, "SELECT * FROM c").unwrap();
    let pair = |a: &str, b: &str| Value::Array(arr(vec![s(a), s(b)]));
    for c in [
        s("nothing"),
        s(""),
        Value::Null,
        i(1),
        pair("self", "foo"),
        pair("bar", "instanceMethod"),
        pair("bar", "privateStatic"),
        pair("bar", "nonexistent"),
    ] {
        match h.fetch_all(&st, Some(FETCH_FUNC), &[c]) {
            Ok(v) => h.dump(&v),
            Err(e) => print_err(&mut h, &e),
        }
    }
    assert_phpt!(h, "pdo_fetch_function_incorrect_callables.phpt");
}

#[test]
fn fetch_lazy_default_and_not_default() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "pdo_fetch_lazy_as_default_changed",
        "id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10), grp VARCHAR(10)",
        &[
            "1, 'A', 'alpha', 'Group1'",
            "2, 'B', 'beta', 'Group1'",
            "3, 'C', 'gamma', 'Group2'",
            "4, 'D', 'delta', 'Group2'",
        ],
    );
    h.add_class(Class::new("Test").prop("val1").prop("val2"));
    let q = h
        .prepare(
            &db,
            "SELECT val2, val1 FROM pdo_fetch_lazy_as_default_changed",
        )
        .unwrap();
    h.execute(&q, None).unwrap();
    h.set_fetch_mode(&q, FETCH_LAZY, &[]).unwrap();
    let o = h.new_object("Test");
    let r = h.fetch(&q, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    drop(r);
    let r = h.fetch_object(&q, Some("Test"), None).unwrap();
    h.dump(&r);
    let r = h.fetch(&q, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    h.set_fetch_mode(&q, FETCH_INTO, &[o.clone()]).unwrap();
    let r = h.fetch(&q, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    h.dump(&o);
    assert_phpt!(h, "pdo_fetch_lazy_as_default.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "pdo_fetch_lazy_as_default_changed",
        "id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10), grp VARCHAR(10)",
        &[
            "1, 'A', 'alpha', 'Group1'",
            "2, 'B', 'beta', 'Group1'",
            "3, 'C', 'gamma', 'Group2'",
            "4, 'D', 'delta', 'Group2'",
        ],
    );
    h.add_class(Class::new("Test").prop("val1").prop("val2"));
    let q = h
        .prepare(
            &db,
            "SELECT val2, val1 FROM pdo_fetch_lazy_as_default_changed",
        )
        .unwrap();
    h.execute(&q, None).unwrap();
    let o = h.new_object("Test");
    h.set_fetch_mode(&q, FETCH_INTO, &[o]).unwrap();
    let r = h.fetch(&q, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    let r = h.fetch(&q, FETCH_LAZY).unwrap();
    h.dump(&r);
    let r = h.fetch(&q, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    let another = h.new_object("stdClass");
    h.dump(&another);
    let r = h.fetch(&q, FETCH_LAZY).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_lazy_as_not_default.phpt");
}

#[test]
fn query_fetch_lazy001() {
    let mut h = TestHost::new();
    let db = h.factory();
    rows(
        &mut h,
        &db,
        "pdo_query_fetch_lazy_001",
        "id int, name varchar(10)",
        &["1,'test1'", "2,'test2'"],
    );
    let r = h
        .query_mode(
            &db,
            "SELECT * FROM pdo_query_fetch_lazy_001",
            Some(FETCH_LAZY),
            &[],
        )
        .unwrap();
    h.dump(&r);
    h.echo("End\n");
    assert_phpt!(h, "pdo_query_fetch_lazy001.phpt");
}

#[test]
fn gh20553_magic_set_fetch_class() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.add_class(
        Class::new("dumpy")
            .ctor(|h, this, args| {
                let class = TestHost::obj(this).class_name().to_string_lossy();
                let a = args.map(|a| Value::Array(a.clone())).unwrap_or(Value::Null);
                let exported = h.export(&a);
                h.echo(&format!(
                    "constructor called,\n  my class is '{class}'\n  input parameters: {exported}\n"
                ));
                Ok(())
            })
            .setter(|h, _, name, value| {
                let v = h.export(&value);
                h.echo(&format!("'{}' = {v}\n", name.to_string_lossy()));
                Ok(())
            }),
    );
    h.add_class(Class::new("dummy").extends("dumpy"));
    let sql = "SELECT 'dummy' pdo_fetch_class_type_class, 'bar' foo, 'dfg' abc";
    for (label, mode) in [
        ("PDO::FETCH_CLASS", FETCH_CLASS),
        (
            "PDO::FETCH_CLASS | PDO::FETCH_PROPS_LATE",
            FETCH_CLASS | FETCH_PROPS_LATE,
        ),
        (
            "PDO::FETCH_CLASS | PDO::FETCH_CLASSTYPE",
            FETCH_CLASS | FETCH_CLASSTYPE,
        ),
        (
            "PDO::FETCH_CLASS | PDO::FETCH_CLASSTYPE | PDO::FETCH_PROPS_LATE",
            FETCH_CLASS | FETCH_CLASSTYPE | FETCH_PROPS_LATE,
        ),
    ] {
        h.echo(&format!("## {label}\n"));
        let q = h.query(&db, sql).unwrap();
        h.fetch_all(
            &q,
            Some(mode),
            &[
                s("dumpy"),
                Value::Array(arr(vec![s("constructor argument #1")])),
            ],
        )
        .unwrap();
        h.echo("\n");
    }
    assert_phpt!(h, "gh20553.phpt");
}

#[test]
fn gh8626_error_info_after_constraint() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_SILENT)).unwrap();
    h.exec(&db, "CREATE TABLE test8626 (x int NOT NULL)")
        .unwrap();
    let st = h.prepare(&db, "INSERT INTO test8626 VALUES(?)").unwrap();
    for (n, v) in [Value::Null, i(1)].into_iter().enumerate() {
        let r = h.execute(&st, Some(arr(vec![v]))).unwrap();
        h.dump(&Value::Bool(r));
        let c = TestHost::stmt(&st).error_code(&mut h).unwrap();
        h.dump(&c);
        let info = h.stmt_error_info(&st).unwrap();
        h.dump(&info);
        if n == 0 {
            h.close_cursor(&st).unwrap();
        }
    }
    h.echo("===DONE===\n");
    assert_phpt!(h, "gh8626.phpt");
}

#[test]
fn gh16703_fetch_into_self() {
    let mut h = TestHost::new();
    h.add_class(Class::new("TestStmt").extends("PDOStatement").prop("name"));
    let mut opts = ZArray::new();
    opts.make_mut()
        .insert(php_runtime::ArrayKey::Int(ATTR_CASE), i(CASE_LOWER));
    opts.make_mut().insert(
        php_runtime::ArrayKey::Int(ATTR_STATEMENT_CLASS),
        Value::Array(arr(vec![s("TestStmt")])),
    );
    let db = h.new_pdo("PDO", "sqlite::memory:", Some(opts)).unwrap();
    h.exec(&db, "CREATE TABLE gh16703 (name varchar(255))")
        .unwrap();
    h.exec(&db, "INSERT INTO gh16703 (name) VALUES ('test_name')")
        .unwrap();
    let st = h.query(&db, "SELECT name FROM gh16703").unwrap();
    h.set_fetch_mode(&st, FETCH_INTO, &[st.clone()]).unwrap();
    h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.echo("done!\n");
    assert_phpt!(h, "gh16703.phpt");
}

#[test]
fn bindcolumn_unknown_column() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE u (name varchar(255))").unwrap();
    let var = php_runtime::PhpRef::new(Value::Null);
    for mode in [ERRMODE_SILENT, ERRMODE_WARNING, ERRMODE_EXCEPTION] {
        h.set_attr(&db, ATTR_ERRMODE, i(mode)).unwrap();
        let st = h.query(&db, "SELECT name FROM u").unwrap();
        if let Err(e) = h.bind_column(&st, ParamId::Name(z("nosuchcolumn")), &var, PARAM_STR) {
            print_err(&mut h, &e);
        }
    }
    let st = h.query(&db, "SELECT name FROM u").unwrap();
    let r = h
        .bind_column(&st, ParamId::Name(z("name")), &var, PARAM_STR)
        .unwrap();
    h.dump(&Value::Bool(r));
    assert_phpt!(h, "pdo_bindcolumn_unknown_column.phpt");
}

#[test]
fn execute_array_binding_failure() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.add_class(Class::new("ThrowingString").method(
        "__toString",
        false,
        Vis::Public,
        |_, _, _| {
            Err(RuntimeError::Custom {
                class: z("RuntimeException"),
                message: "conversion failed".into(),
            })
        },
    ));
    rows(
        &mut h,
        &db,
        "f",
        "id int, name varchar(10)",
        &["1, 'a'", "2, 'b'"],
    );
    let st = h
        .prepare(&db, "SELECT name FROM f WHERE id = :id AND name = :name")
        .unwrap();
    let id = php_runtime::PhpRef::new(i(1));
    let name = php_runtime::PhpRef::new(s("a"));
    h.bind_param(&st, ParamId::Name(z(":id")), &id, PARAM_STR)
        .unwrap();
    h.bind_param(&st, ParamId::Name(z(":name")), &name, PARAM_STR)
        .unwrap();
    let ts = h.new_object("ThrowingString");
    if let Err(e) = h.execute(&st, Some(assoc(vec![(":id", i(2)), (":name", ts)]))) {
        print_err(&mut h, &e);
    }
    let dump = TestHost::stmt(&st).debug_dump_bytes();
    let dump = String::from_utf8_lossy(&dump);
    let n = dump
        .lines()
        .find_map(|l| l.strip_prefix("Params:"))
        .unwrap()
        .trim()
        .to_string();
    h.echo(&format!("bound params after failure: {n}\n"));
    let r = h
        .execute(&st, Some(assoc(vec![(":id", i(2)), (":name", s("b"))])))
        .unwrap();
    h.dump(&Value::Bool(r));
    let r = h.fetch_all(&st, Some(FETCH_COLUMN), &[]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_execute_array_binding_failure.phpt");
}

#[test]
fn oracle_nulls_invalid() {
    let mut h = TestHost::new();
    let db = h.factory();
    for v in [NULL_NATURAL, NULL_EMPTY_STRING, NULL_TO_STRING] {
        h.echo("Valid mode: ");
        let r = h.set_attr(&db, ATTR_ORACLE_NULLS, i(v)).unwrap();
        h.dump(&Value::Bool(r));
        let r = h.get_attr(&db, ATTR_ORACLE_NULLS).unwrap();
        h.dump(&r);
    }
    for v in [-1, 3, 4, i64::MAX] {
        if let Err(e) = h.set_attr(&db, ATTR_ORACLE_NULLS, i(v)) {
            print_err(&mut h, &e);
        }
        h.echo("Unchanged mode: ");
        let r = h.get_attr(&db, ATTR_ORACLE_NULLS).unwrap();
        h.dump(&r);
    }
    assert_phpt!(h, "pdo_oracle_nulls_invalid.phpt");
}

#[test]
fn quote_empty_string() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    let r = h.quote(&db, "", PARAM_STR).unwrap();
    assert_eq!(sv(&r), "''");
    h.echo("DONE\n");
    assert_phpt!(h, "pdo_quote_empty_string.phpt");
}

#[test]
fn debug_emulated_prepares() {
    emu::register();
    let mut h = TestHost::new();
    let db = h.new_pdo("PDO", "sqlemu::memory:", None).unwrap();
    for (a, v) in [
        (ATTR_ERRMODE, i(ERRMODE_WARNING)),
        (ATTR_CASE, i(CASE_LOWER)),
        (ATTR_STRINGIFY_FETCHES, Value::Bool(true)),
        (ATTR_EMULATE_PREPARES, Value::Bool(true)),
    ] {
        h.set_attr(&db, a, v).unwrap();
    }
    let st = h.query(&db, "SELECT 1").unwrap();
    let r = h.debug_dump(&st).unwrap();
    h.dump(&r);
    let st = h
        .prepare(&db, "SELECT :bool, :int, :string, :null")
        .unwrap();
    h.bind_value(
        &st,
        ParamId::Name(z(":bool")),
        Value::Bool(true),
        PARAM_BOOL,
    )
    .unwrap();
    h.bind_value(&st, ParamId::Name(z(":int")), i(123), PARAM_INT)
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":string")), s("foo"), PARAM_STR)
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":null")), Value::Null, PARAM_NULL)
        .unwrap();
    assert!(h.execute(&st, None).unwrap());
    let r = h.debug_dump(&st).unwrap();
    h.dump(&r);
    assert_phpt!(h, "debug_emulated_prepares.phpt");
    let r = h.fetch(&st, FETCH_NUM).unwrap();
    h.out.clear();
    h.dump(&r);
    assert_eq!(
        h.output(),
        "array(4) {\n  [0]=>\n  string(1) \"1\"\n  [1]=>\n  string(3) \"123\"\n  [2]=>\n  string(3) \"foo\"\n  [3]=>\n  NULL\n}\n"
    );
}

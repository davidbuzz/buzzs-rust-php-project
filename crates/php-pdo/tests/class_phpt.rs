//! Hand ports of `ext/pdo/tests/attr_statement_class/*.phpt`,
//! `pecl_bug_*.phpt` and the constructor tests over `sqlite::memory:`.

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
use php_runtime::{ArrayKey, RuntimeError, Value, ZArray};

fn print_err(h: &mut TestHost, e: &RuntimeError) {
    let d = h.describe_error(e);
    h.echo(&format!("{d}\n"));
}

fn label_table(h: &mut TestHost, db: &Value, table: &str) {
    h.exec(
        db,
        &format!("CREATE TABLE {table} (id INT, label CHAR(1), PRIMARY KEY(id))"),
    )
    .unwrap();
    h.exec(
        db,
        &format!("INSERT INTO {table} (id, label) VALUES (1, 'a')"),
    )
    .unwrap();
    h.exec(
        db,
        &format!("INSERT INTO {table} (id, label) VALUES (2, 'b')"),
    )
    .unwrap();
}

fn stmt_class(name: &str, parent: &str, dump_this: bool) -> Class {
    let n = name.to_string();
    Class::new(name)
        .extends(parent)
        .private_ctor()
        .ctor(move |h, this, args| {
            if dump_this {
                h.echo(&format!("{n}::__construct\n"));
                h.dump(this);
            }
            let v = args
                .and_then(|a| a.get_int(0).cloned())
                .unwrap_or(Value::Null);
            h.dump(&v);
            Ok(())
        })
}

fn sc(class: &str, args: Option<Vec<Value>>) -> Value {
    let mut v = vec![s(class)];
    if let Some(a) = args {
        v.push(Value::Array(arr(a)));
    }
    Value::Array(arr(v))
}

#[test]
fn attr_statement_class_basic() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_STRINGIFY_FETCHES, Value::Bool(true))
        .unwrap();
    let table = "pdo_attr_statement_class_basic";
    label_table(&mut h, &db, table);
    let d = h.get_attr(&db, ATTR_STATEMENT_CLASS).unwrap();
    h.dump(&d);
    h.add_class(Class::new("StatementWithPublicDestructor").extends("PDOStatement"));
    for v in [
        sc("StatementWithPublicDestructor", Some(vec![])),
        sc("StatementWithPublicDestructor", None),
    ] {
        match h.set_attr(&db, ATTR_STATEMENT_CLASS, v) {
            Ok(b) => h.dump(&Value::Bool(b)),
            Err(e) => print_err(&mut h, &e),
        }
    }
    let q = format!("SELECT id, label FROM {table} ORDER BY id ASC");
    let st = h.query(&db, &q).unwrap();
    assert_eq!(
        TestHost::obj(&st).class_name().to_string_lossy(),
        "StatementWithPublicDestructor"
    );
    drop(st);
    // the VM runs the destructor on unset($stmt)
    h.echo("StatementWithPublicDestructor::__destruct\n");
    h.echo("Class derived from PDOStatement, with private constructor:\n");
    h.add_class(stmt_class(
        "StatementWithPrivateConstructor",
        "PDOStatement",
        true,
    ));
    let r = h
        .set_attr(
            &db,
            ATTR_STATEMENT_CLASS,
            sc("StatementWithPrivateConstructor", Some(vec![s("param1")])),
        )
        .unwrap();
    h.dump(&Value::Bool(r));
    h.query(&db, &q).unwrap();
    h.echo("Class derived from a child of PDOStatement:\n");
    h.add_class(Class::new("StatementDerivedFromChild").extends("StatementWithPrivateConstructor"));
    let r = h
        .set_attr(
            &db,
            ATTR_STATEMENT_CLASS,
            sc("StatementDerivedFromChild", Some(vec![s("param1")])),
        )
        .unwrap();
    h.dump(&Value::Bool(r));
    let st = h.query(&db, &q).unwrap();
    assert_eq!(
        TestHost::obj(&st).class_name().to_string_lossy(),
        "StatementDerivedFromChild"
    );
    // the overriding fetchAll() returns []
    h.dump(&Value::Array(ZArray::new()));
    h.echo("Reset to default PDOStatement class:\n");
    let r = h
        .set_attr(&db, ATTR_STATEMENT_CLASS, sc("PDOStatement", None))
        .unwrap();
    h.dump(&Value::Bool(r));
    let st = h.query(&db, &q).unwrap();
    let r = h.fetch_all(&st, None, &[]).unwrap();
    h.dump(&r);
    assert_phpt!(
        h,
        "pdo",
        "attr_statement_class/pdo_ATTR_STATEMENT_CLASS_basic.phpt"
    );
}

#[test]
fn attr_statement_class_ctor_args() {
    for (file, prepare) in [
        ("pdo_ATTR_STATEMENT_CLASS_ctor_arg_no_gc.phpt", false),
        (
            "pdo_ATTR_STATEMENT_CLASS_ctor_arg_no-gc_var_modified.phpt",
            false,
        ),
        (
            "pdo_ATTR_STATEMENT_CLASS_ctor_arg_no-gc_var_unset.phpt",
            false,
        ),
        ("pdo_prepare_ATTR_STATEMENT_CLASS_ctor_arg_no-gc.phpt", true),
    ] {
        let mut h = TestHost::new();
        h.add_class(stmt_class("Foo", "PDOStatement", false));
        let db = h.factory();
        let mut a = arr(vec![s("Foo"), Value::Array(arr(vec![s("param1")]))]);
        h.set_attr(&db, ATTR_STATEMENT_CLASS, Value::Array(a.clone()))
            .unwrap();
        // `$a[0] = 'Bar'` must not affect the stored attribute
        a.make_mut().insert(ArrayKey::Int(0), s("Bar"));
        label_table(&mut h, &db, "t");
        let q = "SELECT id, label FROM t ORDER BY id ASC";
        let st = if prepare {
            h.prepare(&db, q).unwrap()
        } else {
            h.query(&db, q).unwrap()
        };
        let is_foo = h.is_a(&TestHost::obj(&st).class_name().to_string_lossy(), "Foo");
        h.dump(&Value::Bool(is_foo));
        let qs = TestHost::get_prop(&st, "queryString").unwrap();
        h.dump(&Value::Bool(
            qs.as_str().unwrap().as_bytes() == q.as_bytes(),
        ));
        if prepare {
            let r = h.execute(&st, None).unwrap();
            h.dump(&Value::Bool(r));
        }
        assert_phpt!(h, "pdo", &format!("attr_statement_class/{file}"));
    }
}

#[test]
fn attr_statement_class_ctor_arg_gc() {
    for (file, prepare) in [
        ("pdo_ATTR_STATEMENT_CLASS_ctor_arg_gc.phpt", false),
        ("pdo_prepare_ATTR_STATEMENT_CLASS_ctor_arg_gc.phpt", true),
    ] {
        let mut h = TestHost::new();
        h.add_class(stmt_class("Foo", "PDOStatement", false));
        h.add_class(Class::new("Bar").extends("PDO").prop_default(
            "statementClass",
            Vis::Public,
            s("Foo"),
        ));
        let mut opts = ZArray::new();
        opts.make_mut()
            .insert(ArrayKey::Int(ATTR_ERRMODE), i(ERRMODE_EXCEPTION));
        let db = h.new_pdo("Bar", "sqlite::memory:", Some(opts)).unwrap();
        h.set_attr(&db, ATTR_STATEMENT_CLASS, sc("Foo", Some(vec![db.clone()])))
            .unwrap();
        for (a, v) in [
            (ATTR_ERRMODE, i(ERRMODE_SILENT)),
            (ATTR_ERRMODE, i(ERRMODE_WARNING)),
            (ATTR_CASE, i(CASE_LOWER)),
            (ATTR_STRINGIFY_FETCHES, Value::Bool(true)),
        ] {
            h.set_attr(&db, a, v).unwrap();
        }
        label_table(&mut h, &db, "t");
        let q = "SELECT id, label FROM t ORDER BY id ASC";
        let st = if prepare {
            h.prepare(&db, q).unwrap()
        } else {
            h.query(&db, q).unwrap()
        };
        let is_foo = h.is_a(&TestHost::obj(&st).class_name().to_string_lossy(), "Foo");
        h.dump(&Value::Bool(is_foo));
        let qs = TestHost::get_prop(&st, "queryString").unwrap();
        h.dump(&Value::Bool(
            qs.as_str().unwrap().as_bytes() == q.as_bytes(),
        ));
        if prepare {
            let r = h.execute(&st, None).unwrap();
            h.dump(&Value::Bool(r));
        }
        assert_phpt!(h, "pdo", &format!("attr_statement_class/{file}"));
    }
}

#[test]
fn attr_statement_class_cyclic_ctor_args() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_STRINGIFY_FETCHES, Value::Bool(true))
        .unwrap();
    label_table(&mut h, &db, "pdo_attr_statement_class_cyclic_ctor_args");
    let d = h.get_attr(&db, ATTR_STATEMENT_CLASS).unwrap();
    h.dump(&d);
    h.add_class(
        Class::new("HoldPdo")
            .extends("PDOStatement")
            .typed("v", "PDO")
            .private_ctor()
            .ctor(|h, this, args| {
                let v = args.unwrap().get_int(0).unwrap().clone();
                TestHost::set_prop(this, "v", v.clone());
                h.dump(&v);
                Ok(())
            }),
    );
    let r = h
        .set_attr(
            &db,
            ATTR_STATEMENT_CLASS,
            sc("HoldPdo", Some(vec![db.clone()])),
        )
        .unwrap();
    h.dump(&Value::Bool(r));
    let st = h
        .query(
            &db,
            "SELECT id, label FROM pdo_attr_statement_class_cyclic_ctor_args ORDER BY id ASC",
        )
        .unwrap();
    h.dump(&st);
    assert_phpt!(
        h,
        "pdo",
        "attr_statement_class/pdo_ATTR_STATEMENT_CLASS_cyclic_ctor_args.phpt"
    );
}

#[test]
fn attr_statement_class_errors() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.add_class(Class::new("NotDerived"));
    h.add_class(
        Class::new("DerivedWithPublicConstructor")
            .extends("PDOStatement")
            .ctor(|_, _, _| Ok(())),
    );
    let attempt = |h: &mut TestHost, v: Value| {
        if let Err(e) = h.set_attr(&db, ATTR_STATEMENT_CLASS, v) {
            print_err(h, &e);
        }
    };
    attempt(&mut h, s("not an array"));
    h.echo("Unknown class variation:\n");
    attempt(&mut h, sc("classname", None));
    attempt(&mut h, sc("classname", Some(vec![])));
    h.echo("Class not derived from PDOStatement:\n");
    attempt(&mut h, sc("NotDerived", None));
    attempt(&mut h, sc("NotDerived", Some(vec![])));
    h.echo("Class derived from PDOStatement, but with public constructor:\n");
    attempt(&mut h, sc("DerivedWithPublicConstructor", None));
    attempt(&mut h, sc("DerivedWithPublicConstructor", Some(vec![])));
    assert_phpt!(
        h,
        "pdo",
        "attr_statement_class/pdo_ATTR_STATEMENT_CLASS_errors.phpt"
    );
}

#[test]
fn attr_statement_class_with_abstract_class() {
    let mut h = TestHost::new();
    let db = h.factory();
    label_table(&mut h, &db, "a");
    let mut c = Class::new("DerivedButAbstract")
        .extends("PDOStatement")
        .private_ctor()
        .ctor(|_, _, _| Ok(()));
    c.is_abstract = true;
    h.add_class(c);
    let r = h
        .set_attr(
            &db,
            ATTR_STATEMENT_CLASS,
            sc("DerivedButAbstract", Some(vec![s("DerivedButAbstract")])),
        )
        .unwrap();
    h.dump(&Value::Bool(r));
    if let Err(e) = h.query(&db, "SELECT id, label FROM a ORDER BY id ASC") {
        print_err(&mut h, &e);
    }
    assert_phpt!(
        h,
        "pdo",
        "attr_statement_class/pdo_ATTR_STATEMENT_CLASS_with_abstract_class.phpt"
    );
}

#[test]
fn persistent_statement_class() {
    let mut h = TestHost::new();
    let mut opts = ZArray::new();
    opts.make_mut()
        .insert(ArrayKey::Int(ATTR_PERSISTENT), Value::Bool(true));
    let db = h.factory_class("PDO", Some(opts));
    let r = h
        .set_attr(&db, ATTR_STATEMENT_CLASS, sc("PDOStatement", None))
        .unwrap();
    h.dump(&Value::Bool(r));
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_EXCEPTION)).unwrap();
    if let Err(e) = h.set_attr(&db, ATTR_STATEMENT_CLASS, sc("PDOStatement", None)) {
        print_err(&mut h, &e);
    }
    assert_phpt!(h, "pdo_persistent_statement_class.phpt");
}

#[test]
fn constructor_option_argnum() {
    let mut h = TestHost::new();
    for connect in [false, true] {
        for (attr, v) in [
            (ATTR_ERRMODE, i(999)),
            (ATTR_STATEMENT_CLASS, s("PDOStatement")),
        ] {
            let mut opts = ZArray::new();
            opts.make_mut().insert(ArrayKey::Int(attr), v);
            let r = if connect {
                h.connect("PDO", "sqlite::memory:", Some(opts))
            } else {
                h.new_pdo("PDO", "sqlite::memory:", Some(opts))
            };
            if let Err(e) = r {
                print_err(&mut h, &e);
            }
        }
        let mut opts = ZArray::new();
        opts.make_mut()
            .insert(ArrayKey::Int(ATTR_DEFAULT_FETCH_MODE), i(FETCH_NUM));
        let db = if connect {
            h.connect("PDO", "sqlite::memory:", Some(opts)).unwrap()
        } else {
            h.new_pdo("PDO", "sqlite::memory:", Some(opts)).unwrap()
        };
        if connect {
            assert_eq!(
                TestHost::obj(&db).class_name().to_string_lossy(),
                "Pdo\\Sqlite"
            );
        }
        let m = h.get_attr(&db, ATTR_DEFAULT_FETCH_MODE).unwrap();
        h.dump(&Value::Bool(matches!(m, Value::Int(FETCH_NUM))));
    }
    assert_phpt!(h, "pdo_constructor_option_argnum.phpt");
}

#[test]
fn sensitive_parameter_and_drivers() {
    let mut h = TestHost::new();
    let e = h.new_pdo("PDO", "dsn", None).unwrap_err();
    assert_eq!(TestHost::error_class(&e), "PDOException");
    assert_eq!(
        TestHost::error_message(&e),
        "PDO::__construct(): Argument #1 ($dsn) must be a valid data source name"
    );
    let drivers = php_pdo::Pdo::get_available_drivers();
    assert!(drivers
        .values()
        .any(|v| v.as_str().is_some_and(|s| s.as_bytes() == b"sqlite")));
    let e = h.new_pdo("PDO", "nosuch:foo", None).unwrap_err();
    assert_eq!(TestHost::error_message(&e), "could not find driver");
}

#[test]
fn pecl_bugs() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test5772 (id int NOT NULL, PRIMARY KEY (id))",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO test5772 (id) VALUES (1)").unwrap();
    h.add_function("heLLO", |_, a| Ok(a[0].clone()));
    let q = h.query(&db, "SELECT * FROM test5772").unwrap();
    let r = h.fetch_all(&q, Some(FETCH_FUNC), &[s("hello")]).unwrap();
    for v in r.as_array().unwrap().values().cloned().collect::<Vec<_>>() {
        h.dump(&v);
    }
    assert_phpt!(h, "pecl_bug_5772.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE test5809 (id int NOT NULL, PRIMARY KEY (id))",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO test5809 (id) VALUES (1)").unwrap();
    let values = arr(vec![i(1)]);
    h.dump(&Value::Array(values.clone()));
    let st = h
        .prepare(&db, "SELECT * FROM test5809 WHERE id = ?")
        .unwrap();
    h.execute(&st, Some(values.clone())).unwrap();
    h.dump(&Value::Array(values));
    assert_phpt!(h, "pecl_bug_5809.phpt");
}

fn statement_x(h: &mut TestHost, set_assoc: bool) {
    h.add_class(
        Class::new("PDOStatementX")
            .extends("PDOStatement")
            .prop("dbh")
            .private_ctor()
            .ctor(move |h, this, args| {
                let dbh = args.unwrap().get_int(0).unwrap().clone();
                TestHost::set_prop(this, "dbh", dbh);
                if set_assoc {
                    h.set_fetch_mode(this, FETCH_ASSOC, &[])?;
                }
                h.echo("PDOStatementX::__construct()\n");
                Ok(())
            }),
    );
    h.add_class(Class::new("PDODatabase").extends("PDO"));
}

fn insert_data(h: &mut TestHost, db: &Value, table: &str) {
    h.exec(
        db,
        &format!(
            "CREATE TABLE {table}(id INT NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(16))"
        ),
    )
    .unwrap();
    let st = h
        .prepare(db, &format!("INSERT INTO {table} VALUES(?, ?, ?)"))
        .unwrap();
    let class = TestHost::obj(&st).class_name().to_string_lossy();
    h.dump(&s(&class));
    for row in [
        ["10", "Abc", "zxy"],
        ["20", "Def", "wvu"],
        ["30", "Ghi", "tsr"],
    ] {
        h.execute(&st, Some(arr(row.iter().map(|v| s(v)).collect())))
            .unwrap();
    }
}

#[test]
fn pdo_026_and_030_statement_class_with_pdo_arg() {
    // pdo_026: PDODatabase::query() prepares with ATTR_STATEMENT_CLASS
    let mut h = TestHost::new();
    statement_x(&mut h, false);
    let db = h.factory_class("PDODatabase", None);
    h.dump(&s("PDODatabase"));
    insert_data(&mut h, &db, "test026");
    h.echo("PDODatabase::query()\n");
    let mut opts = ZArray::new();
    opts.make_mut().insert(
        ArrayKey::Int(ATTR_STATEMENT_CLASS),
        sc("PDOStatementx", Some(vec![db.clone()])),
    );
    let st = h
        .prepare_opts(&db, "SELECT * FROM test026", Some(opts))
        .unwrap();
    h.set_fetch_mode(&st, FETCH_ASSOC, &[]).unwrap();
    h.execute(&st, None).unwrap();
    let class = TestHost::obj(&st).class_name().to_string_lossy();
    h.dump(&s(&class));
    let dbh = TestHost::get_prop(&st, "dbh").unwrap();
    let dbh_class = TestHost::obj(&dbh).class_name().to_string_lossy();
    h.dump(&s(&dbh_class));
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    h.echo("===DONE===\n");
    // destructors run by the VM at shutdown
    h.echo("PDOStatementX::__destruct()\nPDODatabase::__destruct()\n");
    assert_phpt!(h, "pdo_026.phpt");

    // pdo_030: the statement class set as attribute, used by query()
    let mut h = TestHost::new();
    statement_x(&mut h, true);
    let db = h.factory_class("PDODatabase", None);
    h.dump(&s("PDODatabase"));
    insert_data(&mut h, &db, "test030");
    h.echo("===QUERY===\n");
    let a = h.get_attr(&db, ATTR_STATEMENT_CLASS).unwrap();
    h.dump(&a);
    h.set_attr(
        &db,
        ATTR_STATEMENT_CLASS,
        sc("PDOStatementx", Some(vec![db.clone()])),
    )
    .unwrap();
    let a = h.get_attr(&db, ATTR_STATEMENT_CLASS).unwrap();
    h.dump(&a);
    let bad = Value::Array(arr(vec![s("PDOStatement"), s("invalid-args")]));
    if let Err(e) = h.set_attr(&db, ATTR_STATEMENT_CLASS, bad.clone()) {
        print_err(&mut h, &e);
    }
    let a = h.get_attr(&db, ATTR_STATEMENT_CLASS).unwrap();
    h.dump(&a);
    let mut opts = ZArray::new();
    opts.make_mut()
        .insert(ArrayKey::Int(ATTR_STATEMENT_CLASS), bad);
    if let Err(e) = h.prepare_opts(&db, "SELECT * FROM test030", Some(opts)) {
        print_err(&mut h, &e);
    }
    h.echo("PDODatabase::query()\n");
    let st = h.query(&db, "SELECT * FROM test030").unwrap();
    let class = TestHost::obj(&st).class_name().to_string_lossy();
    h.dump(&s(&class));
    let dbh = TestHost::get_prop(&st, "dbh").unwrap();
    let dbh_class = TestHost::obj(&dbh).class_name().to_string_lossy();
    h.dump(&s(&dbh_class));
    h.echo("===FOREACH===\n");
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    h.echo("===DONE===\n");
    h.echo("PDOStatementX::__destruct()\nPDODatabase::__destruct()\n");
    assert_phpt!(h, "pdo_030.phpt");
}

#[test]
fn pdo_drivers_basic() {
    let drivers = php_pdo::Pdo::get_available_drivers();
    assert!(!drivers.is_empty());
    let mut h = TestHost::new();
    h.dump(&Value::Bool(true));
    h.dump(&Value::Bool(true));
    assert_phpt!(h, "pdo_drivers_basic.phpt");
}

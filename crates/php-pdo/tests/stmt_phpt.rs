//! Hand ports of the `PDOStatement` behaviour tests of `ext/pdo/tests`
//! (fetch-mode setup, statement classes, reentrancy) over
//! `sqlite::memory:`.

mod common;

use common::*;
use php_pdo::consts::*;
use php_runtime::{ArrayKey, RuntimeError, Value, ZArray};
use std::rc::Rc;

fn print_err(h: &mut TestHost, e: &RuntimeError) {
    let d = h.describe_error(e);
    h.echo(&format!("{d}\n"));
}

fn sv(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string_lossy(),
        Value::Int(n) => n.to_string(),
        _ => String::new(),
    }
}

fn print_bool_or_err(h: &mut TestHost, r: Result<bool, RuntimeError>) {
    match r {
        Ok(b) => h.dump(&Value::Bool(b)),
        Err(e) => print_err(h, &e),
    }
}

#[test]
fn set_fetch_mode_errors() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE e(id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10))",
    )
    .unwrap();
    let st = h.prepare(&db, "SELECT id, val, val2 from e").unwrap();
    for flags in [0, FETCH_PROPS_LATE] {
        for mode in FETCH_KEY_PAIR..=16 {
            h.echo(&format!("Mode: {mode}\n"));
            let r = h.set_fetch_mode(&st, mode | flags, &[]);
            print_bool_or_err(&mut h, r);
        }
    }
    for mode in [
        FETCH_KEY_PAIR | FETCH_PROPS_LATE,
        FETCH_KEY_PAIR | (FETCH_SERIALIZE * 2),
        FETCH_FUNC,
    ] {
        let r = h.set_fetch_mode(&st, mode, &[]);
        print_bool_or_err(&mut h, r);
    }
    assert_phpt!(h, "pdo_set_fetch_mode_errors.phpt");
}

fn fetch_class_marker(h: &mut TestHost, name: &str) {
    h.add_class(
        Class::new(name)
            .typed("i", "int")
            .typed("marker", "string")
            .ctor(|_, this, args| {
                let m = args
                    .and_then(|a| a.get_int(0).cloned())
                    .unwrap_or(Value::Null);
                TestHost::set_prop(this, "marker", m);
                Ok(())
            }),
    );
}

#[test]
fn setfetchmode_atomic() {
    let mut h = TestHost::new();
    let pdo = h.factory();
    fetch_class_marker(&mut h, "FetchClass");
    h.exec(&pdo, "CREATE TABLE a (i INT)").unwrap();
    h.exec(&pdo, "INSERT INTO a (i) VALUES (1)").unwrap();
    let st = h.query(&pdo, "SELECT i FROM a").unwrap();
    let row = h.new_object("stdClass");
    h.set_fetch_mode(&st, FETCH_INTO, &[row.clone()]).unwrap();
    if let Err(e) = h.set_fetch_mode(&st, FETCH_CLASS, &[]) {
        h.echo(&format!("into error: {}\n", TestHost::error_class(&e)));
    }
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.echo("into object preserved: ");
    let same = matches!((&r, &row), (Value::Object(a), Value::Object(b)) if php_runtime::Obj::ptr_eq(a, b));
    h.dump(&Value::Bool(same));
    let iv = sv(&TestHost::get_prop(&row, "i").unwrap());
    h.echo(&format!("into value: {iv}\n"));

    let st = h.query(&pdo, "SELECT i FROM a").unwrap();
    h.set_fetch_mode(
        &st,
        FETCH_CLASS,
        &[s("FetchClass"), Value::Array(arr(vec![s("kept")]))],
    )
    .unwrap();
    if let Err(e) = h.set_fetch_mode(&st, FETCH_CLASS, &[]) {
        h.echo(&format!("class error: {}\n", TestHost::error_class(&e)));
    }
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    let class = TestHost::obj(&r).class_name().to_string_lossy();
    h.echo(&format!("class preserved: {class}\n"));
    let m = sv(&TestHost::get_prop(&r, "marker").unwrap());
    h.echo(&format!("constructor argument: {m}\n"));
    let iv = sv(&TestHost::get_prop(&r, "i").unwrap());
    h.echo(&format!("class value: {iv}\n"));
    assert_phpt!(h, "pdo_setfetchmode_atomic.phpt");
}

#[test]
fn setfetchmode_reentrant_fetch() {
    let mut h = TestHost::new();
    let pdo = h.factory();
    fetch_class_marker(&mut h, "PriorFetchClass");
    h.exec(&pdo, "CREATE TABLE r (i INT)").unwrap();
    h.exec(&pdo, "INSERT INTO r (i) VALUES (1)").unwrap();
    h.exec(&pdo, "INSERT INTO r (i) VALUES (2)").unwrap();

    let st = h.query(&pdo, "SELECT i FROM r ORDER BY i").unwrap();
    let into = h.new_object("stdClass");
    h.set_fetch_mode(&st, FETCH_INTO, &[into.clone()]).unwrap();
    let (st2, into2) = (st.clone(), into.clone());
    h.autoload = Some(Rc::new(move |h: &mut TestHost, class| {
        let row = h.fetch(&st2, FETCH_DEFAULT).unwrap();
        h.echo("into re-entry preserved: ");
        let same = matches!((&row, &into2), (Value::Object(a), Value::Object(b)) if php_runtime::Obj::ptr_eq(a, b));
        h.dump(&Value::Bool(same));
        let name = class.to_string_lossy();
        h.add_class(Class::new(&name).typed("i", "int"));
    }));
    h.set_fetch_mode(&st, FETCH_CLASS, &[s("ReenteredIntoClass")])
        .unwrap();
    h.autoload = None;
    let row = h.fetch(&st, FETCH_DEFAULT).unwrap();
    let iv = sv(&TestHost::get_prop(&into, "i").unwrap());
    h.echo(&format!("into re-entry value: {iv}\n"));
    let class = TestHost::obj(&row).class_name().to_string_lossy();
    let iv = sv(&TestHost::get_prop(&row, "i").unwrap());
    h.echo(&format!("outer into class: {class} {iv}\n"));

    let st = h.query(&pdo, "SELECT i FROM r ORDER BY i").unwrap();
    h.set_fetch_mode(
        &st,
        FETCH_CLASS,
        &[s("PriorFetchClass"), Value::Array(arr(vec![s("kept")]))],
    )
    .unwrap();
    let st2 = st.clone();
    h.autoload = Some(Rc::new(move |h: &mut TestHost, class| {
        let row = h.fetch(&st2, FETCH_DEFAULT).unwrap();
        let c = TestHost::obj(&row).class_name().to_string_lossy();
        let m = sv(&TestHost::get_prop(&row, "marker").unwrap());
        let iv = sv(&TestHost::get_prop(&row, "i").unwrap());
        h.echo(&format!("class re-entry: {c} {m} {iv}\n"));
        let name = class.to_string_lossy();
        h.add_class(Class::new(&name).typed("i", "int"));
    }));
    h.set_fetch_mode(&st, FETCH_CLASS, &[s("ReenteredFetchClass")])
        .unwrap();
    h.autoload = None;
    let row = h.fetch(&st, FETCH_DEFAULT).unwrap();
    let class = TestHost::obj(&row).class_name().to_string_lossy();
    let iv = sv(&TestHost::get_prop(&row, "i").unwrap());
    h.echo(&format!("outer class: {class} {iv}\n"));
    assert_phpt!(h, "pdo_setfetchmode_reentrant_fetch.phpt");
}

#[test]
fn setfetchmode_reentrant_setmode() {
    let mut h = TestHost::new();
    let pdo = h.factory();
    h.exec(&pdo, "CREATE TABLE r (i INT)").unwrap();
    h.exec(&pdo, "INSERT INTO r (i) VALUES (1)").unwrap();
    let st = h.query(&pdo, "SELECT i FROM r").unwrap();
    let first = h.new_object("stdClass");
    let second = h.new_object("stdClass");
    h.set_fetch_mode(&st, FETCH_INTO, &[first.clone()]).unwrap();
    let (st2, second2) = (st.clone(), second.clone());
    h.autoload = Some(Rc::new(move |h: &mut TestHost, class| {
        h.set_fetch_mode(&st2, FETCH_INTO, &[second2.clone()])
            .unwrap();
        let name = class.to_string_lossy();
        h.add_class(Class::new(&name).typed("i", "int"));
    }));
    h.set_fetch_mode(&st, FETCH_CLASS, &[s("ReenteredSetModeClass")])
        .unwrap();
    h.autoload = None;
    let row = h.fetch(&st, FETCH_DEFAULT).unwrap();
    let class = TestHost::obj(&row).class_name().to_string_lossy();
    let iv = sv(&TestHost::get_prop(&row, "i").unwrap());
    h.echo(&format!("{class} {iv}\n"));
    h.echo("first changed: ");
    let b = TestHost::get_prop(&first, "i").is_some();
    h.dump(&Value::Bool(b));
    h.echo("second changed: ");
    let b = TestHost::get_prop(&second, "i").is_some();
    h.dump(&Value::Bool(b));
    assert_phpt!(h, "pdo_setfetchmode_reentrant_setmode.phpt");
}

#[test]
fn stmt_class_ctor_errors() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE c(id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10))",
    )
    .unwrap();
    h.exec(&db, "INSERT INTO c VALUES(1, 'A', 'AA')").unwrap();
    let st = h.prepare(&db, "SELECT id, val, val2 from c").unwrap();
    h.execute(&st, None).unwrap();
    for f in [0, 1] {
        let r = if f == 0 {
            h.set_fetch_mode(&st, FETCH_CLASS, &[s("Unknown")])
                .map(Value::Bool)
        } else {
            h.fetch_all(&st, Some(FETCH_CLASS), &[s("Unknown")])
        };
        match r {
            Ok(v) => h.dump(&v),
            Err(e) => print_err(&mut h, &e),
        }
    }
    assert_phpt!(h, "pdo_stmt_class_ctor_errors_cannot_find_class.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE c(id int NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10))",
    )
    .unwrap();
    h.add_class(
        Class::new("TestBase")
            .prop("id")
            .prop_vis("val", Vis::Protected)
            .prop_vis("val2", Vis::Private),
    );
    let st = h.prepare(&db, "SELECT id, val, val2 from c").unwrap();
    h.execute(&st, None).unwrap();
    let args = Value::Array(arr(vec![i(0)]));
    match h.fetch_all(&st, Some(FETCH_CLASS), &[s("TestBase"), args.clone()]) {
        Ok(v) => h.dump(&v),
        Err(e) => print_err(&mut h, &e),
    }
    let r = h.set_fetch_mode(&st, FETCH_CLASS, &[s("TestBase"), args]);
    print_bool_or_err(&mut h, r);
    assert_phpt!(h, "pdo_stmt_class_ctor_errors.phpt");
}

#[test]
fn stmt_cyclic_references() {
    let mut h = TestHost::new();
    h.add_class(Class::new("Ref").typed("stmt", "CyclicStatement"));
    h.add_class(
        Class::new("CyclicStatement")
            .extends("PDOStatement")
            .typed("ref", "Ref")
            .private_ctor()
            .ctor(|_, this, args| {
                let r = args.unwrap().get_int(0).unwrap().clone();
                TestHost::set_prop(this, "ref", r);
                Ok(())
            }),
    );
    h.add_class(
        Class::new("TestRow")
            .prop("id")
            .prop("val")
            .prop("val2")
            .typed("arg", "string")
            .ctor(|_, this, args| {
                let a = args.unwrap().get_int(0).unwrap().clone();
                TestHost::set_prop(this, "arg", a);
                Ok(())
            }),
    );
    let db = h.factory();
    h.exec(&db, "CREATE TABLE pdo_stmt_cyclic_ref(id INT NOT NULL PRIMARY KEY, val VARCHAR(10), val2 VARCHAR(10))").unwrap();
    for r in ["1, 'A', 'AA'", "2, 'B', 'BB'", "3, 'C', 'CC'"] {
        h.exec(&db, &format!("INSERT INTO pdo_stmt_cyclic_ref VALUES({r})"))
            .unwrap();
    }
    let rf = h.new_object("Ref");
    h.set_attr(
        &db,
        ATTR_STATEMENT_CLASS,
        Value::Array(arr(vec![s("CyclicStatement"), Value::Array(arr(vec![rf]))])),
    )
    .unwrap();
    let sql = "SELECT id, val2, val FROM pdo_stmt_cyclic_ref";
    h.echo("Column fetch:\n");
    let st = h.query(&db, sql).unwrap();
    let r = TestHost::get_prop(&st, "ref").unwrap();
    TestHost::set_prop(&r, "stmt", st.clone());
    h.set_fetch_mode(&st, FETCH_COLUMN, &[i(2)]).unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    h.echo("Class fetch:\n");
    let st = h.query(&db, sql).unwrap();
    h.set_fetch_mode(
        &st,
        FETCH_CLASS,
        &[s("TestRow"), Value::Array(arr(vec![s("Hello world")]))],
    )
    .unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    h.echo("Fetch into:\n");
    let st = h.query(&db, sql).unwrap();
    let o = h.new_object("TestRow");
    TestHost::set_prop(&o, "arg", s("I am being fetch into"));
    h.set_fetch_mode(&st, FETCH_INTO, &[o]).unwrap();
    h.foreach(&st, |h, _, v| h.dump(&v)).unwrap();
    assert_phpt!(h, "pdo_stmt_cyclic_references.phpt");
}

fn foo_bar(h: &mut TestHost, foo_v_first: bool) {
    let mut foo = Class::new("Foo");
    if foo_v_first {
        foo = foo.typed("id", "int").typed("v", "string");
        foo = foo.ctor(|_, this, args| {
            let v = args
                .and_then(|a| a.get_int(0).cloned())
                .unwrap_or(Value::Null);
            TestHost::set_prop(this, "v", v);
            Ok(())
        });
    } else {
        foo = foo.typed("a", "int").typed("id", "int").ctor(|_, this, args| {
            let a = args.and_then(|a| a.get_str(b"a").or_else(|| a.get_int(0)).cloned());
            let Some(a) = a else {
                return Err(RuntimeError::ArgumentCountError(
                    "Too few arguments to function Foo::__construct(), 0 passed and exactly 1 expected".into(),
                ));
            };
            TestHost::set_prop(this, "a", a);
            Ok(())
        });
    }
    h.add_class(foo);
    h.add_class(Class::new("Bar").typed("id", "int"));
}

#[test]
fn stmt_fetchobject_ctor_args() {
    let mut h = TestHost::new();
    let db = h.factory();
    foo_bar(&mut h, false);
    h.exec(
        &db,
        "CREATE TABLE t (id INT, label CHAR(1), PRIMARY KEY(id))",
    )
    .unwrap();
    for r in ["1, 'a'", "2, 'b'", "3, 'c'"] {
        h.exec(&db, &format!("INSERT INTO t (id, label) VALUES ({r})"))
            .unwrap();
    }
    let st = h.prepare(&db, "SELECT id FROM t ORDER BY id ASC").unwrap();
    let a123 = || {
        let mut a = ZArray::new();
        a.make_mut().insert(ArrayKey::from_bytes(b"a"), i(123));
        a
    };
    let cases: Vec<(&str, Option<ZArray>, bool)> = vec![
        ("Foo", None, false),
        ("Foo", Some(ZArray::new()), false),
        ("Foo", Some(a123()), true),
        ("Bar", None, true),
        ("Bar", Some(ZArray::new()), true),
        ("Bar", Some(a123()), false),
    ];
    for (c, args, dump) in cases {
        h.execute(&st, None).unwrap();
        match h.fetch_object(&st, Some(c), args) {
            Ok(v) => {
                if dump {
                    h.dump(&v);
                }
            }
            Err(e) => print_err(&mut h, &e),
        }
    }
    assert_phpt!(h, "pdo_stmt_fetchobject_ctor_args.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    foo_bar(&mut h, true);
    h.exec(
        &db,
        "CREATE TABLE t (id INT, label CHAR(1), PRIMARY KEY(id))",
    )
    .unwrap();
    for r in ["1, 'a'", "2, 'b'", "3, 'c'"] {
        h.exec(&db, &format!("INSERT INTO t (id, label) VALUES ({r})"))
            .unwrap();
    }
    let st = h.prepare(&db, "SELECT id FROM t ORDER BY id ASC").unwrap();
    h.set_fetch_mode(
        &st,
        FETCH_CLASS,
        &[s("Foo"), Value::Array(arr(vec![s("Hello")]))],
    )
    .unwrap();
    h.execute(&st, None).unwrap();
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    match h.fetch_object(&st, Some("Bar"), Some(arr(vec![s("no-args")]))) {
        Ok(v) => h.dump(&v),
        Err(e) => print_err(&mut h, &e),
    }
    let r = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_stmt_fetchobject_ctor_args_after_default.phpt");
}

#[test]
fn fetch_class_change_ctor_args_during_fetch5() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    h.add_class(Class::new("B").ctor(|_, _, _| Ok(())));
    h.exec(
        &db,
        "CREATE TABLE f(id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10))",
    )
    .unwrap();
    for r in [
        "1, 'A', 'alpha'",
        "2, 'B', 'beta'",
        "3, 'C', 'gamma'",
        "4, 'D', 'delta'",
    ] {
        h.exec(&db, &format!("INSERT INTO f VALUES({r})")).unwrap();
    }
    let st = h.prepare(&db, "SELECT val1, val2 FROM f").unwrap();
    h.execute(&st, None).unwrap();
    let st2 = st.clone();
    h.error_handler = Some(Rc::new(move |h: &mut TestHost, msg: &str| {
        h.set_fetch_mode(
            &st2,
            FETCH_CLASS,
            &[s("B"), Value::Array(arr(vec![s(msg)]))],
        )?;
        h.echo(&format!("{msg}\n"));
        Ok(())
    }));
    let r = h.fetch_all(
        &st,
        Some(FETCH_CLASS | FETCH_SERIALIZE),
        &[s("B"), Value::Array(arr(vec![st.clone()]))],
    );
    match r {
        Ok(v) => h.dump(&v),
        Err(e) => print_err(&mut h, &e),
    }
    assert_phpt!(h, "pdo_fetch_class_change_ctor_args_during_fetch5.phpt");
}

#[test]
fn uninitialized_objects() {
    let mut h = TestHost::new();
    h.add_class(Class::new("MyPDO").extends("PDO").ctor(|_, _, _| Ok(())));
    h.add_class(
        Class::new("MyPDOStatement")
            .extends("PDOStatement")
            .ctor(|_, _, _| Ok(())),
    );
    let pdo = h.new_object("MyPDO");
    if let Err(e) = h.query(&pdo, "foo") {
        print_err(&mut h, &e);
    }
    let st = h.new_object("MyPDOStatement");
    if let Err(e) = h.fetch(&st, FETCH_DEFAULT) {
        print_err(&mut h, &e);
    }
    let st = h.new_object("MyPDOStatement");
    if let Err(e) = h.foreach(&st, |_, _, _| {}) {
        print_err(&mut h, &e);
    }
    assert_phpt!(h, "pdo_uninitialized.phpt");
}

#[test]
fn query_string_read_only() {
    // pdo_036: `queryString` may be initialised once on a bare statement
    let st = php_pdo::PdoStatement::new();
    assert!(!st.is_initialized());
    assert!(matches!(
        php_pdo::PdoStatement::check_property_write(b"queryString", false),
        Ok(false)
    ));
    let e = php_pdo::PdoStatement::check_property_write(b"queryString", true).unwrap_err();
    assert_eq!(e.message(), Some("Property queryString is read only"));
    assert!(php_pdo::PdoStatement::check_property_unset(b"queryString").is_err());
    assert!(php_pdo::PdoStatement::check_property_unset(b"other").is_ok());
    let mut h = TestHost::new();
    let e = php_pdo::PdoRow::constructor_error(&mut h);
    assert_eq!(
        TestHost::error_message(&e),
        "You may not create a PDORow manually"
    );
}

#[test]
fn fetch_function_overload_statement_class() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(
        &db,
        "CREATE TABLE o(id int NOT NULL PRIMARY KEY, val VARCHAR(10))",
    )
    .unwrap();
    for r in ["1, 'A'", "2, 'B'", "3, 'C'", "4, 'D'"] {
        h.exec(&db, &format!("INSERT INTO o VALUES ({r})")).unwrap();
    }
    h.add_class(
        Class::new("DerivedStatement")
            .extends("PDOStatement")
            .prop("name")
            .private_ctor()
            .ctor(|h, this, args| {
                let name = args.unwrap().get_int(0).unwrap().clone();
                h.echo(&format!("DerivedStatement::__construct({})\n", sv(&name)));
                TestHost::set_prop(this, "name", name);
                Ok(())
            })
            .method("reTrieve", false, Vis::Public, |h, _, a| {
                let (id, val) = (sv(&a[0]), sv(&a[1]));
                h.echo(&format!("DerivedStatement::reTrieve({id},{val})\n"));
                Ok(s(&format!("{id} => {val}")))
            }),
    );
    let mut opts = ZArray::new();
    opts.make_mut().insert(
        ArrayKey::Int(ATTR_STATEMENT_CLASS),
        Value::Array(arr(vec![
            s("DerivedStatement"),
            Value::Array(arr(vec![s("Overloaded"), db.clone()])),
        ])),
    );
    let d = h
        .prepare_opts(&db, "SELECT id, val FROM o", Some(opts))
        .unwrap();
    let class = TestHost::obj(&d).class_name().to_string_lossy();
    h.dump(&s(&class));
    h.execute(&d, None).unwrap();
    let callable = Value::Array(arr(vec![d.clone(), s("retrieve")]));
    let r = h.fetch_all(&d, Some(FETCH_FUNC), &[callable]).unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_function_overload_statement_class.phpt");
}

#[test]
fn fetch_class_cyclic_ctor_and_opaque() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.add_class(
        Class::new("Test")
            .typed("val1", "string")
            .typed("val2", "string")
            .ctor(|h, _, a| {
                let v = a.unwrap().get_int(0).unwrap().clone();
                h.dump(&v);
                Ok(())
            }),
    );
    h.exec(
        &db,
        "CREATE TABLE c(id int NOT NULL PRIMARY KEY, val1 VARCHAR(10), val2 VARCHAR(10))",
    )
    .unwrap();
    for r in ["1, 'A', 'alpha'", "2, 'B', 'beta'"] {
        h.exec(&db, &format!("INSERT INTO c VALUES({r})")).unwrap();
    }
    // $args = []; $args[] = &$args;
    let r = php_runtime::PhpRef::new(Value::Array(ZArray::new()));
    let mut inner = ZArray::new();
    let _ = inner.make_mut().push(Value::Reference(r.clone()));
    r.set(Value::Array(inner.clone()));
    let args = r.get();
    let st = h.prepare(&db, "SELECT val1, val2 FROM c").unwrap();
    h.set_fetch_mode(
        &st,
        FETCH_CLASS,
        &[s("Test"), Value::Array(arr(vec![args]))],
    )
    .unwrap();
    h.execute(&st, None).unwrap();
    let v = h.fetch(&st, FETCH_DEFAULT).unwrap();
    h.dump(&v);
    // break the cycle for the test's memory checker
    r.set(Value::Null);
    assert_phpt!(h, "pdo_fetch_class_cyclic_ctor.phpt");

    let mut h = TestHost::new();
    let db = h.factory();
    h.add_class(
        Class::new("Directory")
            .typed("path", "string")
            .typed("handle", "mixed"),
    );
    h.exec(
        &db,
        "CREATE TABLE p(id int NOT NULL PRIMARY KEY, path VARCHAR(10))",
    )
    .unwrap();
    for r in ["1, 'AA'", "2, 'BB'", "3, 'CC'"] {
        h.exec(&db, &format!("INSERT INTO p VALUES({r})")).unwrap();
    }
    let st = h.prepare(&db, "SELECT path FROM p").unwrap();
    h.execute(&st, None).unwrap();
    let r = h
        .fetch_all(
            &st,
            Some(FETCH_CLASS),
            &[s("Directory"), Value::Array(ZArray::new())],
        )
        .unwrap();
    h.dump(&r);
    assert_phpt!(h, "pdo_fetch_class_opaque_object.phpt");
}

#[test]
fn bind_reentrant_execute() {
    let mut h = TestHost::new();
    let db = h.factory();
    h.exec(&db, "CREATE TABLE test_bind_reentrant (name varchar(10))")
        .unwrap();
    let st = h
        .prepare(
            &db,
            "SELECT name FROM test_bind_reentrant WHERE name = ? OR name = ?",
        )
        .unwrap();
    let st2 = st.clone();
    h.add_class(Class::new("ReExecute").method(
        "__toString",
        false,
        Vis::Public,
        move |h, _, _| {
            h.execute(&st2, Some(arr(vec![s("x"), s("y")])))?;
            Ok(s("r"))
        },
    ));
    let re = h.new_object("ReExecute");
    h.execute(&st, Some(arr(vec![s("a"), re]))).unwrap();
    let re = h.new_object("ReExecute");
    h.bind_value(&st, php_pdo::ParamId::Position(2), re, PARAM_STR)
        .unwrap();
    h.echo("Done\n");
    assert_phpt!(h, "pdo_bind_reentrant_execute.phpt");
}

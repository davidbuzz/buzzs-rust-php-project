//! Focused tests of individual core behaviours (fetch modes, conversions,
//! attributes, errors, DSN handling) over `sqlite::memory:`.

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
use php_pdo::{ConnectArgs, ParamId, Pdo};
use php_runtime::{ArrayKey, PhpRef, Value, ZArray};

fn db_with_rows(h: &mut TestHost) -> Value {
    let db = h.new_pdo("PDO", "sqlite::memory:", None).unwrap();
    h.exec(&db, "CREATE TABLE t (a INTEGER, b TEXT, c REAL)")
        .unwrap();
    h.exec(&db, "INSERT INTO t VALUES (1, 'x', 1.5)").unwrap();
    h.exec(&db, "INSERT INTO t VALUES (2, '', NULL)").unwrap();
    db
}

fn dump_str(h: &mut TestHost, v: &Value) -> String {
    let before = h.out.len();
    h.dump(v);
    let s = String::from_utf8_lossy(&h.out[before..]).into_owned();
    h.out.truncate(before);
    s
}

#[test]
fn fetch_named_merges_duplicates() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    let q = h
        .query(&db, "SELECT a AS v, b AS v, c AS w FROM t WHERE a = 1")
        .unwrap();
    let r = h.fetch(&q, FETCH_NAMED).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [\"v\"]=>\n  array(2) {\n    [0]=>\n    int(1)\n    [1]=>\n    string(1) \"x\"\n  }\n  [\"w\"]=>\n  float(1.5)\n}\n"
    );
}

#[test]
fn native_types_without_stringify() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    let q = h.query(&db, "SELECT * FROM t").unwrap();
    let r = h.fetch_all(&q, Some(FETCH_NUM), &[]).unwrap();
    let d = dump_str(&mut h, &r);
    assert!(d.contains("int(1)") && d.contains("float(1.5)") && d.contains("NULL"));
    h.set_attr(&db, ATTR_STRINGIFY_FETCHES, Value::Bool(true))
        .unwrap();
    let q = h.query(&db, "SELECT a, c FROM t WHERE a = 1").unwrap();
    let r = h.fetch(&q, FETCH_NUM).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [0]=>\n  string(1) \"1\"\n  [1]=>\n  string(3) \"1.5\"\n}\n"
    );
}

#[test]
fn oracle_nulls_conversions() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    h.set_attr(&db, ATTR_ORACLE_NULLS, i(NULL_EMPTY_STRING))
        .unwrap();
    let q = h.query(&db, "SELECT b, c FROM t WHERE a = 2").unwrap();
    let r = h.fetch(&q, FETCH_NUM).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [0]=>\n  NULL\n  [1]=>\n  NULL\n}\n"
    );
    h.set_attr(&db, ATTR_ORACLE_NULLS, i(NULL_TO_STRING))
        .unwrap();
    let q = h.query(&db, "SELECT b, c FROM t WHERE a = 2").unwrap();
    let r = h.fetch(&q, FETCH_NUM).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [0]=>\n  string(0) \"\"\n  [1]=>\n  string(0) \"\"\n}\n"
    );
}

#[test]
fn bound_column_type_overrides() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    let st = h.prepare(&db, "SELECT a, b, c FROM t WHERE a = 1").unwrap();
    h.execute(&st, None).unwrap();
    let (a, b, c) = (
        PhpRef::new(Value::Null),
        PhpRef::new(Value::Null),
        PhpRef::new(Value::Null),
    );
    h.bind_column(&st, ParamId::Position(1), &a, PARAM_BOOL)
        .unwrap();
    h.bind_column(&st, ParamId::Name(z("b")), &b, PARAM_LOB)
        .unwrap();
    h.bind_column(&st, ParamId::Position(3), &c, PARAM_INT)
        .unwrap();
    assert!(matches!(
        h.fetch(&st, FETCH_BOUND).unwrap(),
        Value::Bool(true)
    ));
    assert!(matches!(a.get(), Value::Bool(true)));
    assert!(matches!(b.get(), Value::Resource(_)));
    assert!(matches!(c.get(), Value::Int(1)));
    // bound columns are 1-based positions
    let e = h
        .bind_column(&st, ParamId::Position(0), &a, PARAM_STR)
        .unwrap_err();
    assert_eq!(
        e.message(),
        Some(
            "PDOStatement::bindColumn(): Argument #1 ($column) must be greater than or equal to 1"
        )
    );
}

#[test]
fn fetch_modes_overview() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    h.set_attr(&db, ATTR_STRINGIFY_FETCHES, Value::Bool(true))
        .unwrap();
    let q = h.query(&db, "SELECT a, b FROM t").unwrap();
    let r = h.fetch_all(&q, Some(FETCH_KEY_PAIR), &[]).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [1]=>\n  string(1) \"x\"\n  [2]=>\n  string(0) \"\"\n}\n"
    );
    let q = h.query(&db, "SELECT a, b FROM t").unwrap();
    let r = h.fetch_all(&q, Some(FETCH_COLUMN), &[i(1)]).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [0]=>\n  string(1) \"x\"\n  [1]=>\n  string(0) \"\"\n}\n"
    );
    let q = h.query(&db, "SELECT a, b FROM t").unwrap();
    let r = h.fetch(&q, FETCH_OBJ).unwrap();
    assert!(TestHost::get_prop(&r, "b").is_some());
    // invalid modes
    let q = h.query(&db, "SELECT a FROM t").unwrap();
    let e = h.fetch(&q, 13).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::fetch(): Argument #1 ($mode) must be a bitmask of PDO::FETCH_* constants")
    );
    let e = h.fetch(&q, FETCH_FUNC).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::fetch(): Argument #1 ($mode) PDO::FETCH_FUNC can only be used with PDOStatement::fetchAll()")
    );
    let e = h.fetch_all(&q, Some(FETCH_LAZY), &[]).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::fetchAll(): Argument #1 ($mode) PDO::FETCH_LAZY cannot be used with PDOStatement::fetchAll()")
    );
    let e = h.fetch_all(&q, Some(FETCH_NUM), &[i(1)]).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::fetchAll() expects exactly 1 argument for the fetch mode provided, 2 given")
    );
    let e = h.fetch_all(&q, Some(FETCH_COLUMN), &[s("x")]).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::fetchAll(): Argument #2 must be of type int, string given")
    );
    // after the end, fetch returns false without an error
    let q = h.query(&db, "SELECT a FROM t WHERE 0").unwrap();
    assert!(matches!(
        h.fetch(&q, FETCH_NUM).unwrap(),
        Value::Bool(false)
    ));
    let info = h.stmt_error_info(&q).unwrap();
    assert_eq!(
        dump_str(&mut h, &info),
        "array(3) {\n  [0]=>\n  string(5) \"00000\"\n  [1]=>\n  NULL\n  [2]=>\n  NULL\n}\n"
    );
}

#[test]
fn iterator_keys() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    let q = h.query(&db, "SELECT a FROM t").unwrap();
    h.set_fetch_mode(&q, FETCH_COLUMN, &[i(0)]).unwrap();
    let mut keys = Vec::new();
    h.foreach(&q, |_, k, v| keys.push((k.as_int(), v.as_int())))
        .unwrap();
    assert_eq!(keys, vec![(Some(0), Some(1)), (Some(1), Some(2))]);
    let e = TestHost::stmt(&q).iterator(&mut h, true).unwrap_err();
    assert_eq!(
        e.message(),
        Some("An iterator cannot be used with foreach by reference")
    );
}

#[test]
fn exec_quote_last_insert_id() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    assert!(matches!(
        h.exec(&db, "UPDATE t SET b = 'y' WHERE 0").unwrap(),
        Value::Int(0)
    ));
    assert!(matches!(
        h.exec(&db, "UPDATE t SET b = 'y'").unwrap(),
        Value::Int(2)
    ));
    let e = h.exec(&db, "").unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDO::exec(): Argument #1 ($statement) must not be empty")
    );
    let q = h.quote(&db, "it's", PARAM_STR).unwrap();
    assert_eq!(q.as_str().unwrap().as_bytes(), b"'it''s'");
    let e = h.quote(&db, "a\0b", PARAM_STR).unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "SQLite PDO::quote does not support null bytes"
    );
    h.exec(&db, "INSERT INTO t VALUES (3, 'z', 0)").unwrap();
    let id = TestHost::pdo(&db).last_insert_id(&mut h, None).unwrap();
    assert_eq!(id.as_str().unwrap().as_bytes(), b"3");
}

#[test]
fn errmode_exception_details() {
    let mut h = TestHost::new();
    let db = h.new_pdo("PDO", "sqlite::memory:", None).unwrap();
    let e = h.query(&db, "SELECT * FROM missing").unwrap_err();
    assert_eq!(TestHost::error_class(&e), "PDOException");
    assert_eq!(
        TestHost::error_message(&e),
        "SQLSTATE[HY000]: General error: 1 no such table: missing"
    );
    let code = TestHost::error_prop(&e, "code").unwrap();
    assert_eq!(code.as_str().unwrap().as_bytes(), b"HY000");
    let info = TestHost::error_prop(&e, "errorInfo").unwrap();
    assert_eq!(
        dump_str(&mut h, &info),
        "array(3) {\n  [0]=>\n  string(5) \"HY000\"\n  [1]=>\n  int(1)\n  [2]=>\n  string(22) \"no such table: missing\"\n}\n"
    );
    // impl errors carry a two-element errorInfo
    let e = h.get_attr(&db, 4242).unwrap_err();
    let info = TestHost::error_prop(&e, "errorInfo").unwrap();
    assert_eq!(info.as_array().unwrap().len(), 2);
    assert_eq!(
        TestHost::error_message(&e),
        "SQLSTATE[IM001]: Driver does not support this function: driver does not support that attribute"
    );
}

#[test]
fn transactions_errors() {
    let mut h = TestHost::new();
    let db = h.new_pdo("PDO", "sqlite::memory:", None).unwrap();
    let pdo = TestHost::pdo(&db);
    let e = pdo.commit(&mut h).unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "There is no active transaction"
    );
    assert!(pdo.begin_transaction(&mut h).unwrap());
    assert!(pdo.in_transaction(&mut h).unwrap());
    let e = pdo.begin_transaction(&mut h).unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "There is already an active transaction"
    );
    assert!(pdo.roll_back(&mut h).unwrap());
    assert!(!pdo.in_transaction(&mut h).unwrap());
    // a transaction left open is rolled back when the object goes away
    h.exec(&db, "CREATE TABLE x (a)").unwrap();
    pdo.begin_transaction(&mut h).unwrap();
    h.exec(&db, "INSERT INTO x VALUES (1)").unwrap();
}

#[test]
fn attributes() {
    let mut h = TestHost::new();
    let db = h.new_pdo("PDO", "sqlite::memory:", None).unwrap();
    let name = h.get_attr(&db, ATTR_DRIVER_NAME).unwrap();
    assert_eq!(name.as_str().unwrap().as_bytes(), b"sqlite");
    assert!(matches!(
        h.get_attr(&db, ATTR_ERRMODE).unwrap(),
        Value::Int(ERRMODE_EXCEPTION)
    ));
    assert!(matches!(
        h.get_attr(&db, ATTR_DEFAULT_FETCH_MODE).unwrap(),
        Value::Int(FETCH_BOTH)
    ));
    assert!(matches!(
        h.get_attr(&db, ATTR_PERSISTENT).unwrap(),
        Value::Bool(false)
    ));
    assert!(matches!(
        h.get_attr(&db, ATTR_SERVER_VERSION).unwrap(),
        Value::Str(_)
    ));
    let e = h.set_attr(&db, ATTR_ERRMODE, i(7)).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDO::setAttribute(): Argument #2 ($value) Error mode must be one of the PDO::ERRMODE_* constants")
    );
    let e = h.set_attr(&db, ATTR_CASE, s("abc")).unwrap_err();
    assert_eq!(
        e.message(),
        Some("Attribute value must be of type int for selected attribute, string given")
    );
    assert!(h.set_attr(&db, ATTR_CASE, s("2")).unwrap());
    let e = h
        .set_attr(
            &db,
            ATTR_DEFAULT_FETCH_MODE,
            Value::Array(arr(vec![i(FETCH_CLASS)])),
        )
        .unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDO::setAttribute(): Argument #2 ($value) PDO::FETCH_INTO and PDO::FETCH_CLASS cannot be set as the default fetch mode")
    );
    let e = h.set_attr(&db, ATTR_DEFAULT_FETCH_MODE, i(0)).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDO::setAttribute(): Argument #2 ($value) Fetch mode must be a bitmask of PDO::FETCH_* constants")
    );
    // unsupported driver attribute: plain false
    assert!(!h.set_attr(&db, ATTR_AUTOCOMMIT, i(0)).unwrap());
    let st = h.prepare(&db, "SELECT 1").unwrap();
    let v = TestHost::stmt(&st)
        .get_attribute(&mut h, ATTR_EMULATE_PREPARES)
        .unwrap();
    assert!(matches!(v, Value::Bool(false)));
    let e = TestHost::stmt(&st).get_attribute(&mut h, 4242).unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "SQLSTATE[IM001]: Driver does not support this function: driver doesn't support getting that attribute"
    );
}

#[test]
fn column_meta_and_rowsets() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    let st = h.prepare(&db, "SELECT a, b FROM t").unwrap();
    let stmt = TestHost::stmt(&st);
    // not executed yet: plain false
    assert!(matches!(
        stmt.get_column_meta(&mut h, 0).unwrap(),
        Value::Bool(false)
    ));
    h.execute(&st, None).unwrap();
    let m = stmt.get_column_meta(&mut h, 1).unwrap();
    let keys: Vec<String> = m
        .as_array()
        .unwrap()
        .keys()
        .map(|k| k.to_zstr().to_string_lossy())
        .collect();
    assert_eq!(
        keys,
        [
            "native_type",
            "pdo_type",
            "sqlite:decl_type",
            "table",
            "flags",
            "name",
            "len",
            "precision"
        ]
    );
    let e = stmt.get_column_meta(&mut h, -1).unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::getColumnMeta(): Argument #1 ($column) must be greater than or equal to 0")
    );
    let e = stmt.get_column_meta(&mut h, 5).unwrap_err();
    assert!(TestHost::error_message(&e).starts_with("SQLSTATE[HY000]"));
    let e = stmt.next_rowset(&mut h).unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "SQLSTATE[IM001]: Driver does not support this function: driver does not support multiple rowsets"
    );
    assert!(stmt.close_cursor(&mut h).unwrap());
    assert_eq!(stmt.column_count(&mut h).unwrap(), 2);
}

#[test]
fn bind_param_names_and_errors() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    let st = h.prepare(&db, "SELECT b FROM t WHERE a = :a").unwrap();
    let e = h
        .bind_value(&st, ParamId::Name(z("")), i(1), PARAM_INT)
        .unwrap_err();
    assert_eq!(
        e.message(),
        Some("PDOStatement::bindValue(): Argument #1 ($param) must not be empty")
    );
    // names without ':' are normalised
    h.bind_value(&st, ParamId::Name(z("a")), i(1), PARAM_INT)
        .unwrap();
    h.execute(&st, None).unwrap();
    let v = h.fetch_column(&st, 0).unwrap();
    assert_eq!(v.as_str().unwrap().as_bytes(), b"x");
    // PARAM_STR converts the bound variable immediately
    let var = PhpRef::new(i(5));
    h.bind_param(&st, ParamId::Name(z(":a")), &var, PARAM_STR)
        .unwrap();
    assert!(matches!(var.get(), Value::Str(_)));
    // PARAM_INPUT_OUTPUT flags are kept (as a C int)
    let d = TestHost::stmt(&st).debug_dump_bytes();
    let d = String::from_utf8_lossy(&d).into_owned();
    assert!(d.contains("param_type=2"), "{d}");
    h.bind_param(
        &st,
        ParamId::Name(z(":a")),
        &var,
        PARAM_STR | PARAM_INPUT_OUTPUT,
    )
    .unwrap();
    let d = String::from_utf8_lossy(&TestHost::stmt(&st).debug_dump_bytes()).into_owned();
    assert!(d.contains("param_type=-2147483646"), "{d}");
}

#[test]
fn dsn_alias_uri_and_connect() {
    let mut h = TestHost::new();
    h.ini
        .insert(b"pdo.dsn.mydb".to_vec(), b"sqlite::memory:".to_vec());
    let db = h.new_pdo("PDO", "mydb", None).unwrap();
    assert!(TestHost::pdo(&db).driver_name() == Some("sqlite"));
    h.ini.insert(b"pdo.dsn.bad".to_vec(), b"nocolon".to_vec());
    let e = h.new_pdo("PDO", "bad", None).unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "invalid data source name (via INI: pdo.dsn.bad)"
    );
    // uri: without a readable stream
    let e = h
        .new_pdo("PDO", "uri:file:///nonexistent", None)
        .unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "PDO::__construct(): Argument #1 ($dsn) must be a valid data source URI"
    );
    assert!(h
        .output()
        .contains("Deprecated: Looking up the DSN from a URI is deprecated"));
    common::emu::register();
    // PDO::connect() picks the driver subclass
    let v = h.connect("PDO", "sqlite::memory:", None).unwrap();
    assert_eq!(
        TestHost::obj(&v).class_name().to_string_lossy(),
        "Pdo\\Sqlite"
    );
    let e = h
        .connect("Pdo\\Sqlite", "sqlemu::memory:", None)
        .unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "Pdo\\Sqlite::connect() cannot be used for connecting to an unknown driver, call PDO::connect() instead"
    );
    // a driver-specific class constructed for the wrong driver
    let e = h
        .new_pdo("Pdo\\Sqlite", "sqlemu::memory:", None)
        .unwrap_err();
    assert_eq!(
        TestHost::error_message(&e),
        "Pdo\\Sqlite::__construct() cannot be used for connecting to an unknown driver, call PDO::__construct() instead"
    );
}

#[test]
fn persistent_connections_are_shared() {
    let mut h = TestHost::new();
    let mut o = ZArray::new();
    o.make_mut()
        .insert(ArrayKey::Int(ATTR_PERSISTENT), s("mykey"));
    let a = h
        .new_pdo("PDO", "sqlite::memory:", Some(o.clone()))
        .unwrap();
    h.exec(&a, "CREATE TABLE p (x)").unwrap();
    let b = h.new_pdo("PDO", "sqlite::memory:", Some(o)).unwrap();
    // same connection: the table is visible
    h.exec(&b, "INSERT INTO p VALUES (1)").unwrap();
    assert!(h
        .persistent
        .contains(b"PDO:DBH:DSN=sqlite::memory::::mykey"));
    assert!(matches!(
        h.get_attr(&b, ATTR_PERSISTENT).unwrap(),
        Value::Bool(true)
    ));
    drop(a);
    drop(b);
    h.persistent.shutdown();
    assert!(h.persistent.is_empty());
}

#[test]
fn uninitialized_pdo_handle() {
    let mut h = TestHost::new();
    let p = Pdo::new();
    let e = p.exec(&mut h, &z("SELECT 1")).unwrap_err();
    assert_eq!(e.message(), Some("PDO object is uninitialized"));
    let e = p
        .construct(&mut h, &z("PDO"), ConnectArgs::dsn(b"sqlite::memory:"))
        .and_then(|()| p.exec(&mut h, &z("")));
    assert!(e.is_err());
    assert!(p.driver_name() == Some("sqlite"));
}

fn emu_db(h: &mut TestHost) -> Value {
    common::emu::register();
    let db = h.new_pdo("PDO", "sqlemu::memory:", None).unwrap();
    h.set_attr(&db, ATTR_ERRMODE, i(ERRMODE_WARNING)).unwrap();
    db
}

#[test]
fn emulated_prepares() {
    let mut h = TestHost::new();
    let db = emu_db(&mut h);
    let st = h.prepare(&db, "SELECT ?, ?").unwrap();
    assert!(h.execute(&st, Some(arr(vec![s("it's"), i(5)]))).unwrap());
    let d = String::from_utf8_lossy(&TestHost::stmt(&st).debug_dump_bytes()).into_owned();
    assert!(d.contains("Sent SQL: [19] SELECT 'it''s', '5'"), "{d}");
    let r = h.fetch(&st, FETCH_NUM).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [0]=>\n  string(4) \"it's\"\n  [1]=>\n  string(1) \"5\"\n}\n"
    );
    // wrong number of parameters
    let st = h.prepare(&db, "SELECT ?").unwrap();
    assert!(!h.execute(&st, Some(arr(vec![i(1), i(2)]))).unwrap());
    assert!(h.output().contains(
        "PDOStatement::execute(): SQLSTATE[HY093]: Invalid parameter number: number of bound variables does not match number of tokens"
    ));
    // mixed placeholders
    h.out.clear();
    let st = h.prepare(&db, "SELECT :a, ?").unwrap();
    assert!(!h.execute(&st, Some(arr(vec![i(1)]))).unwrap());
    assert!(h
        .output()
        .contains("Invalid parameter number: mixed named and positional parameters"));
    // undefined named parameter
    h.out.clear();
    let st = h.prepare(&db, "SELECT :a").unwrap();
    assert!(!h.execute(&st, Some(assoc(vec![(":b", i(1))]))).unwrap());
    assert!(
        h.output()
            .contains("Invalid parameter number: parameter was not defined"),
        "{}",
        h.output()
    );
    // the same named parameter twice
    h.out.clear();
    let st = h.prepare(&db, "SELECT :a, :a").unwrap();
    assert!(h.execute(&st, Some(assoc(vec![(":a", s("v"))]))).unwrap());
    let r = h.fetch(&st, FETCH_NUM).unwrap();
    assert_eq!(
        dump_str(&mut h, &r),
        "array(2) {\n  [0]=>\n  string(1) \"v\"\n  [1]=>\n  string(1) \"v\"\n}\n"
    );
    // PDO::ATTR_EMULATE_PREPARES as seen by the statement
    let v = TestHost::stmt(&st)
        .get_attribute(&mut h, ATTR_EMULATE_PREPARES)
        .unwrap();
    assert!(matches!(v, Value::Bool(true)));
    // typed parameters
    let st = h.prepare(&db, "SELECT :i, :b, :n, :l").unwrap();
    h.bind_value(&st, ParamId::Name(z(":i")), s("12abc"), PARAM_INT)
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":b")), i(0), PARAM_BOOL)
        .unwrap();
    h.bind_value(&st, ParamId::Name(z(":n")), s("x"), PARAM_NULL)
        .unwrap();
    let lob = Value::Resource(php_runtime::Resource::new(
        "stream",
        Some(Box::new(b"blob".to_vec())),
    ));
    h.bind_value(&st, ParamId::Name(z(":l")), lob, PARAM_LOB)
        .unwrap();
    assert!(h.execute(&st, None).unwrap());
    let d = String::from_utf8_lossy(&TestHost::stmt(&st).debug_dump_bytes()).into_owned();
    assert!(
        d.contains("Sent SQL: [26] SELECT 12, 0, NULL, 'blob'"),
        "{d}"
    );
}

#[test]
fn gc_values_report_bound_state() {
    let mut h = TestHost::new();
    let db = db_with_rows(&mut h);
    h.set_attr(
        &db,
        ATTR_STATEMENT_CLASS,
        Value::Array(arr(vec![s("PDOStatement")])),
    )
    .unwrap();
    assert!(TestHost::pdo(&db).gc_values().is_empty());
    let st = h.prepare(&db, "SELECT a FROM t WHERE a = ?").unwrap();
    let v = PhpRef::new(i(1));
    h.bind_param(&st, ParamId::Position(1), &v, PARAM_INT)
        .unwrap();
    let o = h.new_object("stdClass");
    h.set_fetch_mode(&st, FETCH_INTO, std::slice::from_ref(&o))
        .unwrap();
    let gc = TestHost::stmt(&st).gc_values();
    // the PDO object, the bound variable and the fetch-into object
    assert_eq!(gc.len(), 3);
}

#[test]
fn constructor_turns_warnings_into_exceptions() {
    let mut h = TestHost::new();
    let mut o = ZArray::new();
    o.make_mut()
        .insert(ArrayKey::Int(ATTR_PERSISTENT), Value::Bool(true));
    o.make_mut()
        .insert(ArrayKey::Int(ATTR_ERRMODE), i(ERRMODE_WARNING));
    o.make_mut().insert(
        ArrayKey::Int(ATTR_STATEMENT_CLASS),
        Value::Array(arr(vec![s("PDOStatement")])),
    );
    let e = h.new_pdo("PDO", "sqlite::memory:", Some(o)).unwrap_err();
    assert_eq!(TestHost::error_class(&e), "PDOException");
    assert_eq!(
        TestHost::error_message(&e),
        "PDO::__construct(): SQLSTATE[HY000]: General error: PDO::ATTR_STATEMENT_CLASS cannot be used with persistent PDO instances"
    );
    assert!(matches!(
        TestHost::error_prop(&e, "code"),
        Some(Value::Int(0))
    ));
    // nothing was printed
    assert_eq!(h.output(), "");
}

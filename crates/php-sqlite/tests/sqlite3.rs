//! Ports of `ext/sqlite3/tests/*.phpt` expectations.

mod common;

use common::{dump, s, TestHost};
use php_sqlite::constants::*;
use php_sqlite::{ParamKey, Sqlite3};

fn mem(h: &mut TestHost) -> Sqlite3 {
    Sqlite3::construct(
        h,
        b":memory:",
        SQLITE3_OPEN_READWRITE | SQLITE3_OPEN_CREATE,
        b"",
    )
    .expect("open")
}

#[test]
fn create_insert_select() {
    let mut h = TestHost::new();
    let db = mem(&mut h);
    assert!(db
        .exec(&mut h, b"CREATE TABLE test (time INTEGER, id STRING)")
        .unwrap());
    h.at("SQLite3::exec");
    assert!(!db
        .exec(&mut h, b"CREATE TABLE test (time INTEGER, id STRING)")
        .unwrap());
    assert_eq!(
        h.take_log(),
        ["Warning: SQLite3::exec(): table test already exists"]
    );
    assert!(db
        .exec(&mut h, b"INSERT INTO test (time, id) VALUES (1, 'a')")
        .unwrap());
    assert!(db
        .exec(&mut h, b"INSERT INTO test (time, id) VALUES (2, 'b')")
        .unwrap());
    let res = db
        .query(&mut h, b"SELECT * FROM test ORDER BY id ASC", true)
        .unwrap()
        .unwrap();
    let r1 = res.fetch_array(&mut h, SQLITE3_NUM).unwrap();
    assert_eq!(
        dump(&r1),
        "array(2) {\n  [0]=>\n  int(1)\n  [1]=>\n  string(1) \"a\"\n}\n"
    );
    let stmt = db
        .prepare(&mut h, b"SELECT * FROM test WHERE id = ?")
        .unwrap()
        .unwrap();
    assert!(stmt
        .bind_value(ParamKey::Number(1), &s("b"), Some(SQLITE3_TEXT))
        .unwrap());
    let r = stmt.execute(&mut h).unwrap().unwrap();
    let row = r.fetch_array(&mut h, SQLITE3_ASSOC).unwrap();
    assert_eq!(
        dump(&row),
        "array(2) {\n  [\"time\"]=>\n  int(2)\n  [\"id\"]=>\n  string(1) \"b\"\n}\n"
    );
    assert_eq!(
        dump(&r.fetch_array(&mut h, SQLITE3_ASSOC).unwrap()),
        "bool(false)
"
    );
    assert!(db.close(&mut h).unwrap());
}

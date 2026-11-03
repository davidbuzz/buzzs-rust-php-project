//! Prepared statements: parameter binding/encoding, long data, cursors,
//! multiple result sets, errors — compared with `/usr/bin/php`.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::doc_markdown,
    clippy::format_collect
)]

mod common;

use common::*;
use php_mysqlnd::consts::*;
use php_mysqlnd::{ExecuteError, ParamBind};
use php_runtime::error::{ErrorLevel, RtResult, RuntimeContext};
use php_runtime::{PhpRef, Value};

struct Ctx;
impl RuntimeContext for Ctx {
    fn diagnostic(&mut self, _level: ErrorLevel, _message: &str) -> RtResult<()> {
        Ok(())
    }
}

fn s(v: &str) -> Value {
    Value::string(v)
}

#[test]
fn parameter_types_round_trip() {
    let Some(c) = connect() else { return };
    let t = table("stmt_params");
    c.query(format!("DROP TABLE IF EXISTS {t}").as_bytes())
        .unwrap();
    c.query(format!("CREATE TABLE {t} (id INT, i BIGINT, u BIGINT UNSIGNED, d DOUBLE, s VARCHAR(100), b BLOB)").as_bytes())
        .unwrap();
    let st = c.stmt_init();
    st.prepare(format!("INSERT INTO {t} VALUES (?, ?, ?, ?, ?, ?)").as_bytes())
        .unwrap();
    assert_eq!(st.param_count(), 6);
    let rows: Vec<[Value; 6]> = vec![
        [
            Value::Int(1),
            Value::Int(-5),
            s("18446744073709551615"),
            Value::Float(1.25),
            s("hello"),
            s("bin\0ary"),
        ],
        [
            Value::Int(2),
            s("42abc"),
            Value::Float(1e19),
            s("2.5"),
            Value::Int(17),
            Value::Null,
        ],
        [
            Value::Int(3),
            Value::Bool(true),
            Value::Int(7),
            Value::Bool(false),
            Value::Float(0.1),
            s(""),
        ],
        [
            Value::Int(4),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ],
        [
            Value::Int(5),
            Value::Float(9.9e18),
            s("9223372036854775808"),
            Value::Int(3),
            s("x"),
            s("y"),
        ],
    ];
    let types = [
        MYSQL_TYPE_LONGLONG,
        MYSQL_TYPE_LONGLONG,
        MYSQL_TYPE_LONGLONG,
        MYSQL_TYPE_DOUBLE,
        MYSQL_TYPE_VAR_STRING,
        MYSQL_TYPE_VAR_STRING,
    ];
    let mut out = Vec::new();
    for row in rows {
        let binds = row
            .into_iter()
            .zip(types)
            .map(|(v, t)| ParamBind::new(t, v))
            .collect();
        st.bind_params(binds).unwrap();
        let ok = st.execute(&mut Ctx).is_ok();
        out.push(format!(
            "{}|{}|{}|{}",
            u8::from(ok),
            st.errno(),
            st.affected_rows() as i64,
            st.insert_id()
        ));
    }
    // references are read at execute time
    let r = PhpRef::new(Value::Int(100));
    st.bind_params(vec![
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Reference(r.clone())),
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Int(1)),
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Int(1)),
        ParamBind::new(MYSQL_TYPE_DOUBLE, Value::Int(1)),
        ParamBind::new(MYSQL_TYPE_VAR_STRING, Value::Int(1)),
        ParamBind::new(MYSQL_TYPE_VAR_STRING, Value::Int(1)),
    ])
    .unwrap();
    st.execute(&mut Ctx).unwrap();
    r.set(Value::Int(101));
    st.execute(&mut Ctx).unwrap();
    let mut res = query(&c, &format!("SELECT * FROM {t} ORDER BY id")).unwrap();
    while let Some(row) = res.fetch_row().unwrap() {
        out.push(ra(&row));
    }
    c.query(format!("DROP TABLE {t}").as_bytes()).unwrap();
    let mine = out.join("\n");
    if !have_php() {
        return;
    }
    let code = format!(
        r#"$m = conn(); $t = "{t}"; $m->query("DROP TABLE IF EXISTS $t");
        $m->query("CREATE TABLE $t (id INT, i BIGINT, u BIGINT UNSIGNED, d DOUBLE, s VARCHAR(100), b BLOB)");
        $s = $m->prepare("INSERT INTO $t VALUES (?, ?, ?, ?, ?, ?)"); $out = [];
        foreach ([[1, -5, "18446744073709551615", 1.25, "hello", "bin\0ary"], [2, "42abc", 1e19, "2.5", 17, null],
                  [3, true, 7, false, 0.1, ""], [4, null, null, null, null, null], [5, 9.9e18, "9223372036854775808", 3, "x", "y"]] as $row) {{
            [$a, $b, $cc, $d, $e, $f] = $row; $s->bind_param("iiidss", $a, $b, $cc, $d, $e, $f); $ok = $s->execute();
            $out[] = ($ok?1:0)."|".$s->errno."|".$s->affected_rows."|".$s->insert_id;
        }}
        $v = 100; $one = 1; $s->bind_param("iiidss", $v, $one, $one, $one, $one, $one); $s->execute(); $v = 101; $s->execute();
        $r = $m->query("SELECT * FROM $t ORDER BY id"); while ($row = $r->fetch_row()) $out[] = r($row);
        $m->query("DROP TABLE $t"); echo implode("\n", $out);"#
    );
    assert_lines_eq(&mine, &php(&code));
}

#[test]
fn long_data_and_blob_params() {
    let Some(c) = connect() else { return };
    let t = table("stmt_long");
    c.query(format!("DROP TABLE IF EXISTS {t}").as_bytes())
        .unwrap();
    c.query(format!("CREATE TABLE {t} (id INT, b LONGBLOB)").as_bytes())
        .unwrap();
    let st = c.stmt_init();
    st.prepare(format!("INSERT INTO {t} VALUES (?, ?)").as_bytes())
        .unwrap();
    // send_long_data before binding is out of sync
    let e = st.send_long_data(1, b"x").unwrap_err();
    assert_eq!(e.errno, CR_COMMANDS_OUT_OF_SYNC);
    st.bind_params(vec![
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Int(1)),
        ParamBind::new(MYSQL_TYPE_LONG_BLOB, Value::Null),
    ])
    .unwrap();
    let e = st.send_long_data(0, b"x").unwrap_err();
    assert_eq!(
        (e.errno, e.message.as_slice()),
        (CR_INVALID_BUFFER_USE, MSG_NOT_BOUND_AS_BLOB.as_bytes())
    );
    let e = st.send_long_data(5, b"x").unwrap_err();
    assert_eq!(e.errno, CR_INVALID_PARAMETER_NO);
    let chunk: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    st.send_long_data(1, &chunk).unwrap();
    st.send_long_data(1, b"tail").unwrap();
    st.execute(&mut Ctx).unwrap();
    // without long data the blob is sent as an empty string
    st.bind_params(vec![
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Int(2)),
        ParamBind::new(MYSQL_TYPE_LONG_BLOB, Value::Null),
    ])
    .unwrap();
    st.execute(&mut Ctx).unwrap();
    let mut r = query(
        &c,
        &format!("SELECT id, LENGTH(b), MD5(b), b IS NULL FROM {t} ORDER BY id"),
    )
    .unwrap();
    let mut out = Vec::new();
    while let Some(row) = r.fetch_row().unwrap() {
        out.push(ra(&row));
    }
    let mut expect = chunk.clone();
    expect.extend_from_slice(b"tail");
    let md5 = php_hash::bin2hex(&php_hash::md5(&expect));
    assert!(out[0].contains(&h(md5.as_bytes())), "{}", out[0]);
    c.query(format!("DROP TABLE {t}").as_bytes()).unwrap();
}

#[test]
fn statement_errors() {
    let Some(c) = connect() else { return };
    let st = c.stmt_init();
    let e = st.prepare(b"SELEC ?").unwrap_err();
    assert_eq!(e.errno, 1064);
    assert_eq!(st.sqlstate(), "42000");
    assert_eq!(c.errno(), 1064);
    st.prepare(b"SELECT ?, ?").unwrap();
    let ExecuteError::Mysql(e) = st.execute(&mut Ctx).unwrap_err() else {
        panic!()
    };
    assert_eq!(
        (e.errno, e.message.as_slice()),
        (
            CR_PARAMS_NOT_BOUND,
            &b"No data supplied for parameters in prepared statement"[..]
        )
    );
    st.bind_one_param(0, Value::Int(1), MYSQL_TYPE_LONGLONG)
        .unwrap();
    let ExecuteError::Mysql(e) = st.execute(&mut Ctx).unwrap_err() else {
        panic!()
    };
    assert_eq!(
        e.message,
        b"No data supplied for 1 parameter in prepared statement"
    );
    let e = st
        .bind_one_param(2, Value::Int(1), MYSQL_TYPE_LONGLONG)
        .unwrap_err();
    assert_eq!(e.errno, CR_INVALID_PARAMETER_NO);
    st.bind_one_param(1, Value::string("x"), MYSQL_TYPE_VAR_STRING)
        .unwrap();
    st.execute(&mut Ctx).unwrap();
    let row = st.fetch().unwrap().unwrap();
    assert_eq!(rrow(&row), rrow(&[Value::Int(1), Value::string("x")]));
    assert!(st.fetch().unwrap().is_none());
    // fetch on a statement without a result set is out of sync
    let st2 = c.stmt_init();
    st2.prepare(b"DO 1").unwrap();
    st2.execute(&mut Ctx).unwrap();
    let e = st2.fetch().unwrap_err();
    assert_eq!(e.errno, CR_COMMANDS_OUT_OF_SYNC);
    // executing an unprepared statement
    let st3 = c.stmt_init();
    let ExecuteError::Mysql(e) = st3.execute(&mut Ctx).unwrap_err() else {
        panic!()
    };
    assert_eq!(e.errno, CR_COMMANDS_OUT_OF_SYNC);
    // re-prepare reuses the handle
    st.prepare(b"SELECT 7").unwrap();
    st.execute(&mut Ctx).unwrap();
    assert_eq!(rrow(&st.fetch().unwrap().unwrap()), rrow(&[Value::Int(7)]));
}

#[test]
fn cursor_fetch() {
    let Some(c) = connect() else { return };
    let t = table("stmt_cursor");
    c.query(format!("DROP TABLE IF EXISTS {t}").as_bytes())
        .unwrap();
    c.query(format!("CREATE TABLE {t} (id INT, v VARCHAR(10))").as_bytes())
        .unwrap();
    c.query(format!("INSERT INTO {t} VALUES (1,'a'),(2,'b'),(3,'c')").as_bytes())
        .unwrap();
    let st = c.stmt_init();
    st.prepare(format!("SELECT * FROM {t} ORDER BY id").as_bytes())
        .unwrap();
    st.attr_set(STMT_ATTR_CURSOR_TYPE, CURSOR_TYPE_READ_ONLY)
        .unwrap();
    st.execute(&mut Ctx).unwrap();
    // with an open cursor the connection is usable while rows are pending
    let st2 = c.stmt_init();
    st2.prepare(b"SELECT 42").unwrap();
    let mut out = Vec::new();
    while let Some(row) = st.fetch().unwrap() {
        out.push(rrow(&row));
    }
    out.push(format!(
        "status:{}",
        st.server_status() & (SERVER_STATUS_LAST_ROW_SENT | SERVER_STATUS_CURSOR_EXISTS)
    ));
    // store_result with a cursor fetches everything at once
    st.execute(&mut Ctx).unwrap();
    st.store_result().unwrap();
    out.push(format!("num_rows:{}", st.num_rows()));
    while let Some(row) = st.fetch().unwrap() {
        out.push(rrow(&row));
    }
    let stats = c.stats();
    out.push(format!(
        "cursor_rows:{}",
        stats.get(php_mysqlnd::stats::Stat::RowsFetchedFromClientPsCursor)
    ));
    st.close();
    c.query(format!("DROP TABLE {t}").as_bytes()).unwrap();
    let mine = out.join("\n");
    if !have_php() {
        return;
    }
    let code = format!(
        r#"$m = conn(); $t = "{t}"; $m->query("DROP TABLE IF EXISTS $t");
        $m->query("CREATE TABLE $t (id INT, v VARCHAR(10))"); $m->query("INSERT INTO $t VALUES (1,'a'),(2,'b'),(3,'c')");
        $s = $m->stmt_init(); $s->prepare("SELECT * FROM $t ORDER BY id"); $s->attr_set(MYSQLI_STMT_ATTR_CURSOR_TYPE, MYSQLI_CURSOR_TYPE_READ_ONLY); $s->execute(); $s2 = $m->prepare("SELECT 42");
        $a = null; $b = null; $s->bind_result($a, $b); $out = [];
        while ($s->fetch()) $out[] = r([$a, $b]);
        $st = $m->query("SELECT 1"); 
        $out[] = "status:SKIP";
        $s->execute(); $s->store_result(); $out[] = "num_rows:".$s->num_rows;
        while ($s->fetch()) $out[] = r([$a, $b]);
        $out[] = "cursor_rows:".mysqli_get_connection_stats($m)["rows_fetched_from_client_ps_cursor"];
        $s->close(); $m->query("DROP TABLE $t"); echo implode("\n", $out);"#
    );
    let theirs = php(&code);
    let mine_cmp: Vec<&str> = mine.lines().filter(|l| !l.starts_with("status:")).collect();
    let theirs_cmp: Vec<&str> = theirs
        .lines()
        .filter(|l| !l.starts_with("status:"))
        .collect();
    assert_lines_eq(&mine_cmp.join("\n"), &theirs_cmp.join("\n"));
}

#[test]
fn multiple_result_sets_from_procedure() {
    let Some(c) = connect() else { return };
    let p = "mysqlnd_rs_proc";
    c.query(format!("DROP PROCEDURE IF EXISTS {p}").as_bytes())
        .unwrap();
    c.query(format!("CREATE PROCEDURE {p}(IN x INT) BEGIN SELECT x AS a; SELECT x + 1 AS b, 'two' AS c; END").as_bytes())
        .unwrap();
    let mut out = Vec::new();
    // text protocol
    c.query(format!("CALL {p}(5)").as_bytes()).unwrap();
    loop {
        if let Some(mut r) = c.store_result().unwrap() {
            while let Some(row) = r.fetch_assoc().unwrap() {
                out.push(ra(&row));
            }
        } else {
            out.push(format!("upsert:{}", c.affected_rows() as i64));
        }
        if !c.more_results() {
            break;
        }
        assert!(c.next_result().unwrap());
    }
    // binary protocol
    let st = c.stmt_init();
    st.prepare(format!("CALL {p}(?)").as_bytes()).unwrap();
    st.bind_params(vec![ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Int(10))])
        .unwrap();
    st.execute(&mut Ctx).unwrap();
    loop {
        if let Some(mut r) = st.get_result().unwrap() {
            while let Some(row) = r.fetch_assoc().unwrap() {
                out.push(ra(&row));
            }
        } else {
            out.push("ps-upsert".into());
        }
        if !st.more_results() {
            break;
        }
        assert!(st.next_result().unwrap());
    }
    st.close();
    c.query(format!("DROP PROCEDURE {p}").as_bytes()).unwrap();
    let mine = out.join("\n");
    if !have_php() {
        return;
    }
    let code = format!(
        r#"$m = conn(); $p = "{p}"; $m->query("DROP PROCEDURE IF EXISTS $p");
        $m->query("CREATE PROCEDURE $p(IN x INT) BEGIN SELECT x AS a; SELECT x + 1 AS b, 'two' AS c; END");
        $out = []; $m->multi_query("CALL $p(5)");
        do {{ if ($r = $m->store_result()) {{ while ($row = $r->fetch_assoc()) $out[] = r($row); }} else $out[] = "upsert:".$m->affected_rows;
             if (!$m->more_results()) break; }} while ($m->next_result());
        $s = $m->prepare("CALL $p(?)"); $x = 10; $s->bind_param("i", $x); $s->execute();
        do {{ if ($r = $s->get_result()) {{ while ($row = $r->fetch_assoc()) $out[] = r($row); }} else $out[] = "ps-upsert";
             if (!$s->more_results()) break; }} while ($s->next_result());
        $s->close(); $m->query("DROP PROCEDURE $p"); echo implode("\n", $out);"#
    );
    assert_lines_eq(&mine, &php(&code));
}

#[test]
fn integer_overflow_param_resends_types() {
    let Some(c) = connect() else { return };
    let st = c.stmt_init();
    st.prepare(b"SELECT ? AS v, CAST(? AS CHAR) AS t").unwrap();
    let r = PhpRef::new(Value::Int(1));
    st.bind_params(vec![
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Reference(r.clone())),
        ParamBind::new(MYSQL_TYPE_LONGLONG, Value::Reference(r.clone())),
    ])
    .unwrap();
    let mut out = Vec::new();
    for v in [
        Value::Int(1),
        Value::string("99999999999999999999"),
        Value::Int(2),
        Value::Float(-1e30),
        Value::string("12"),
    ] {
        r.set(v);
        st.execute(&mut Ctx).unwrap();
        out.push(rrow(&st.fetch().unwrap().unwrap()));
        assert!(st.fetch().unwrap().is_none());
    }
    let mine = out.join("\n");
    if !have_php() {
        return;
    }
    let theirs = php(
        r#"$m = conn(); $s = $m->prepare("SELECT ? AS v, CAST(? AS CHAR) AS t"); $v = 1; $s->bind_param("ii", $v, $v);
        $out = []; foreach ([1, "99999999999999999999", 2, -1e30, "12"] as $x) { $v = $x; $s->execute(); $a = $b = null; $s->bind_result($a, $b); $s->fetch(); $out[] = r([$a, $b]); $s->fetch(); }
        echo implode("\n", $out);"#,
    );
    assert_lines_eq(&mine, &theirs);
}

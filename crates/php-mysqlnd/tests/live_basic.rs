// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Live tests against the local MariaDB: connection info, errors, simple
//! queries, compared with `/usr/bin/php8.5`'s mysqli on the same server.
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
use php_mysqlnd::Connection;

#[test]
fn connection_info_matches_oracle() {
    let Some(c) = connect() else { return };
    let mine = format!(
        "{}|{}|{}|{}|{}|{}",
        c.server_info().unwrap(),
        c.server_version(),
        c.host_info().unwrap(),
        c.proto_info(),
        c.character_set_name().unwrap(),
        c.charset().unwrap().collation
    );
    assert!(c.thread_id() > 0);
    if !have_php() {
        return;
    }
    let theirs = php(r#"$m = conn(); $cs = $m->get_charset();
        echo $m->server_info, "|", $m->server_version, "|", $m->host_info, "|", $m->protocol_version, "|", $m->character_set_name(), "|", $cs->collation;"#);
    assert_eq!(mine, theirs);
}

#[test]
fn simple_select_text_protocol() {
    let Some(c) = connect() else { return };
    let mut r = query(&c, "SELECT 1 AS a, 'x' AS b, NULL AS c, 1.5 AS d").unwrap();
    assert_eq!(r.num_rows(), 1);
    let row = r.fetch_array(FETCH_BOTH).unwrap().unwrap();
    let mine = ra(&row);
    assert!(r.fetch_row().unwrap().is_none());
    if have_php() {
        let theirs = php(
            r#"$m = conn(); $r = $m->query("SELECT 1 AS a, 'x' AS b, NULL AS c, 1.5 AS d"); echo r($r->fetch_array(MYSQLI_BOTH));"#,
        );
        assert_eq!(mine, theirs);
    }
}

#[test]
fn connect_errors_match_oracle() {
    let p = params();
    let Some(_alive) = connect() else { return };
    let mut mine = Vec::new();
    for (pass, db, port) in [
        ("bad", p.db.as_str(), p.port),
        (p.passwd.as_str(), "nodb_xyz", p.port),
        (p.passwd.as_str(), p.db.as_str(), 1u32),
    ] {
        let c = Connection::init();
        let e = c
            .connect(
                Some(&p.host),
                Some(p.user.as_bytes()),
                Some(pass.as_bytes()),
                Some(db.as_bytes()),
                port,
                None,
                0,
            )
            .unwrap_err();
        mine.push(format!(
            "{}|{}|{}",
            e.errno,
            e.sqlstate,
            String::from_utf8_lossy(&e.message)
        ));
        assert_eq!(c.errno(), e.errno);
    }
    let c = Connection::init();
    let e = c
        .connect(
            Some("localhost"),
            Some(b"x"),
            None,
            None,
            0,
            Some("/nonexistent/mysql.sock"),
            0,
        )
        .unwrap_err();
    mine.push(format!(
        "{}|{}|{}",
        e.errno,
        e.sqlstate,
        String::from_utf8_lossy(&e.message)
    ));
    let mine = mine.join("\n");
    if !have_php() {
        return;
    }
    let code = format!(
        r#"$h="{h}"; $u="{u}"; $p="{pw}"; $d="{d}"; $port={port};
        foreach ([["bad",$d,$port],[$p,"nodb_xyz",$port],[$p,$d,1]] as [$pp,$dd,$po]) {{
            $m = mysqli_init(); @$m->real_connect($h,$u,$pp,$dd,$po);
            $out[] = $m->connect_errno."|HY000|".$m->connect_error;
        }}
        $m = mysqli_init(); @$m->real_connect("localhost","x",null,null,0,"/nonexistent/mysql.sock");
        $out[] = $m->connect_errno."|HY000|".$m->connect_error;
        echo implode("\n", $out);"#,
        h = p.host,
        u = p.user,
        pw = p.passwd,
        d = p.db,
        port = p.port
    );
    assert_eq!(mine, php(&code));
}

#[test]
fn server_errors_and_sqlstate() {
    let Some(c) = connect() else { return };
    let e = c.query(b"SELEC 1").unwrap_err();
    assert_eq!(e.errno, 1064);
    assert_eq!(c.sqlstate(), "42000");
    let mine = format!(
        "{}|{}|{}",
        c.errno(),
        c.sqlstate(),
        String::from_utf8_lossy(&c.error())
    );
    assert_eq!(c.error_list().len(), 1);
    let e2 = c.query(b"SELECT * FROM no_such_table_xyz").unwrap_err();
    let mine2 = format!(
        "{}|{}|{}",
        e2.errno,
        c.sqlstate(),
        String::from_utf8_lossy(&e2.message)
    );
    c.query(b"SELECT 1").unwrap();
    assert_eq!(c.errno(), 0);
    assert_eq!(c.sqlstate(), "00000");
    let _ = c.store_result().unwrap();
    if have_php() {
        let theirs = php(
            r#"$m = conn(); $m->query("SELEC 1"); echo $m->errno,"|",$m->sqlstate,"|",$m->error,"\n";
            $m->query("SELECT * FROM no_such_table_xyz"); echo $m->errno,"|",$m->sqlstate,"|",$m->error;"#,
        );
        assert_eq!(format!("{mine}\n{mine2}"), theirs);
    }
}

#[test]
fn upsert_status_matches_oracle() {
    let Some(c) = connect() else { return };
    let t = table("upsert");
    let script = [
        format!("DROP TABLE IF EXISTS {t}"),
        format!("CREATE TABLE {t} (id INT AUTO_INCREMENT PRIMARY KEY, v VARCHAR(10), n TINYINT)"),
        format!("INSERT INTO {t} (v, n) VALUES ('a', 1), ('b', 2), ('c', 300)"),
        format!("UPDATE {t} SET v = 'z' WHERE id > 1"),
        format!("UPDATE {t} SET v = 'z' WHERE id > 1"),
        format!("INSERT INTO {t} (v) VALUES ('q')"),
        format!("DELETE FROM {t} WHERE id = 1"),
        format!("SELECT * FROM {t}"),
        format!("DROP TABLE {t}"),
    ];
    let mut mine = Vec::new();
    for q in &script {
        let ok = c.query(q.as_bytes()).is_ok();
        if let Some(mut r) = c.store_result().unwrap() {
            r.free(false);
        }
        let aff = c.affected_rows();
        let aff = if aff == u64::MAX {
            -1
        } else {
            i64::try_from(aff).unwrap()
        };
        mine.push(format!(
            "{}|{}|{}|{}|{}",
            u8::from(ok),
            aff,
            c.insert_id(),
            c.warning_count(),
            c.info()
                .map(|i| String::from_utf8_lossy(&i).into_owned())
                .unwrap_or_default()
        ));
    }
    let mine = mine.join("\n");
    if have_php() {
        let qs: Vec<String> = script.iter().map(|q| format!("{q:?}")).collect();
        let code = format!(
            r#"$m = conn(); $out = [];
            foreach ([{}] as $q) {{ $r = $m->query($q); $ok = $r !== false; if ($r instanceof mysqli_result) $r->free();
              $out[] = ($ok?1:0)."|".$m->affected_rows."|".$m->insert_id."|".$m->warning_count."|".$m->info; }}
            echo implode("\n", $out);"#,
            qs.join(",")
        );
        assert_eq!(mine, php(&code));
    }
}

#[test]
fn out_of_sync_with_unbuffered_result() {
    let Some(c) = connect() else { return };
    c.query(b"SELECT 1 UNION SELECT 2").unwrap();
    let mut r = c.use_result().unwrap().unwrap();
    assert_eq!(r.num_rows(), 0);
    let e = c.query(b"SELECT 3").unwrap_err();
    assert_eq!(e.errno, CR_COMMANDS_OUT_OF_SYNC);
    assert_eq!(e.message, MSG_OUT_OF_SYNC.as_bytes());
    assert!(r.fetch_row().unwrap().is_some());
    assert!(r.fetch_row().unwrap().is_some());
    assert!(r.fetch_row().unwrap().is_none());
    assert_eq!(r.num_rows(), 2);
    c.query(b"SELECT 3").unwrap();
    // dropping an unread unbuffered set drains it
    let r2 = c.use_result().unwrap().unwrap();
    drop(r2);
    c.query(b"SELECT 4").unwrap();
    assert!(c.store_result().unwrap().is_some());
}

#[test]
fn ping_select_db_statistics_kill() {
    let Some(c) = connect() else { return };
    c.ping().unwrap();
    assert_eq!(c.affected_rows(), u64::MAX);
    c.select_db(b"test").unwrap();
    // ER_BAD_DB_ERROR, "Unknown database": the test user has
    // `GRANT ALL PRIVILEGES ON *.*`, so MariaDB reports the missing
    // database itself (ext/mysqli/tests/gh22854.phpt) instead of
    // ER_DBACCESS_DENIED_ERROR (1044) for a user without privileges.
    let e = c.select_db(b"nonexistent_db_xyz").unwrap_err();
    assert_eq!(e.errno, 1049);
    let s = c.statistics().unwrap();
    assert!(
        s.starts_with(b"Uptime: "),
        "{}",
        String::from_utf8_lossy(&s)
    );
    // kill another connection
    let Some(other) = connect() else { return };
    #[allow(clippy::cast_possible_truncation)]
    c.kill(other.thread_id() as u32).unwrap();
    let e = other.query(b"SELECT 1").unwrap_err();
    assert!(e.errno == 2006 || e.errno == 2013 || e.errno == 1927, "{e}");
    // kill ourselves: no reply is read, the connection is marked closed
    #[allow(clippy::cast_possible_truncation)]
    c.kill(c.thread_id() as u32).unwrap();
    assert_eq!(c.state(), php_mysqlnd::ConnState::QuitSent);
    let e = c.ping().unwrap_err();
    assert_eq!(e.errno, CR_SERVER_GONE_ERROR);
}

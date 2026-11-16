// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Unix-socket connections, async polling, persistent pooling, session
//! tracking and privileged commands — compared with `/usr/bin/php`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::many_single_char_names,
    clippy::format_collect
)]

mod common;

use common::*;
use php_mysqlnd::{poll, pool, CloseType, Connection};

const SOCKET: &str = "/run/mysqld/mysqld.sock";

#[test]
fn unix_socket_connection() {
    let Some(_alive) = connect() else { return };
    if !std::path::Path::new(SOCKET).exists() {
        return;
    }
    let p = params();
    let c = Connection::init();
    c.connect(
        Some("localhost"),
        Some(p.user.as_bytes()),
        Some(p.passwd.as_bytes()),
        Some(p.db.as_bytes()),
        0,
        Some(SOCKET),
        0,
    )
    .unwrap();
    assert_eq!(c.host_info().as_deref(), Some("Localhost via UNIX socket"));
    assert_eq!(
        c.scheme().as_deref(),
        Some("unix:///run/mysqld/mysqld.sock")
    );
    let mut r = query(&c, "SELECT 1").unwrap();
    assert!(r.fetch_row().unwrap().is_some());
    // async + poll over the unix socket
    c.send_query(b"SELECT SLEEP(0.05)").unwrap();
    let res = poll(std::slice::from_ref(&c), &[], 5, 0).unwrap();
    assert_eq!(res.read, vec![0]);
    c.reap_query().unwrap();
    assert!(c.store_result().unwrap().is_some());
}

#[test]
fn poll_async_queries() {
    let (Some(a), Some(b), Some(idle)) = (connect(), connect(), connect()) else {
        return;
    };
    a.send_query(b"SELECT SLEEP(0.3), 'a'").unwrap();
    b.send_query(b"SELECT 'b'").unwrap();
    let conns = [a.clone(), b.clone(), idle.clone()];
    let res = poll(&conns, &[], 2, 0).unwrap();
    assert_eq!(res.dont_poll, vec![2]);
    assert_eq!(res.read, vec![1], "the fast query finishes first");
    b.reap_query().unwrap();
    let _ = b.store_result().unwrap();
    let res = poll(&conns[..1], &[], 0, 10_000).unwrap();
    assert!(res.read.is_empty(), "sleep(0.3) not done after 10ms");
    let res = poll(&conns[..1], &[], 5, 0).unwrap();
    assert_eq!(res.read, vec![0]);
    a.reap_query().unwrap();
    let _ = a.store_result().unwrap();
    let _ = php_mysqlnd::diag::take();
    assert!(poll(std::slice::from_ref(&idle), &[], 0, 0).is_none());
    assert_eq!(
        php_mysqlnd::diag::take()[0].message,
        "All arrays passed are clear"
    );
    assert!(poll(&[], &[], -1, 0).is_none());
}

#[test]
fn persistent_pool() {
    let Some(c) = connect() else { return };
    let tid = c.thread_id();
    pool::pool_put("p:test", c);
    assert_eq!(pool::pool_len("p:test"), 1);
    let c = pool::pool_take("p:test").unwrap();
    assert_eq!(c.thread_id(), tid);
    assert_eq!(c.stats().get(php_mysqlnd::stats::Stat::ConnectReused), 1);
    c.ping().unwrap();
    assert!(pool::pool_take("p:test").is_none());
    c.close(CloseType::Explicit);
}

#[test]
fn session_tracking() {
    let Some(_alive) = connect() else { return };
    let p = params();
    let c = Connection::init();
    c.enable_session_track(true);
    c.connect(
        Some(&p.host),
        Some(p.user.as_bytes()),
        Some(p.passwd.as_bytes()),
        Some(p.db.as_bytes()),
        p.port,
        None,
        0,
    )
    .unwrap();
    if c.server_capabilities() & php_mysqlnd::consts::CLIENT_SESSION_TRACK == 0 {
        return;
    }
    c.query(b"SET SESSION session_track_system_variables = 'autocommit'")
        .unwrap();
    c.query(b"SET autocommit = 0").unwrap();
    let t = c.session_track();
    assert!(
        t.iter()
            .any(|(kind, data)| *kind == 0 && data.windows(10).any(|w| w == b"autocommit")),
        "{t:?}"
    );
}

#[test]
fn privileged_commands_match_oracle() {
    let Some(c) = connect() else { return };
    let mut out = Vec::new();
    let r = c.refresh(php_mysqlnd::consts::REFRESH_STATUS as u8);
    out.push(format!(
        "{}|{}|{}",
        u8::from(r.is_ok()),
        c.errno(),
        String::from_utf8_lossy(&c.error())
    ));
    let r = c.dump_debug_info();
    out.push(format!(
        "{}|{}|{}",
        u8::from(r.is_ok()),
        c.errno(),
        String::from_utf8_lossy(&c.error())
    ));
    let r = c.kill(999_999);
    out.push(format!(
        "{}|{}|{}",
        u8::from(r.is_ok()),
        c.errno(),
        String::from_utf8_lossy(&c.error())
    ));
    let mine = out.join("\n");
    if have_php() {
        let theirs = php(r#"$m = conn(); $out = [];
            $r = $m->refresh(MYSQLI_REFRESH_STATUS); $out[] = ($r?1:0)."|".$m->errno."|".$m->error;
            $r = $m->dump_debug_info(); $out[] = ($r?1:0)."|".$m->errno."|".$m->error;
            $r = $m->kill(999999); $out[] = ($r?1:0)."|".$m->errno."|".$m->error;
            echo implode("\n", $out);"#);
        assert_lines_eq(&mine, &theirs);
    }
}

#[test]
fn error_and_affected_rows_save_restore() {
    let Some(c) = connect() else { return };
    let _ = c.query(b"SELEC 1");
    let saved = c.error_info();
    let up = c.upsert_status();
    c.query(b"SELECT 1").unwrap();
    let _ = c.store_result().unwrap();
    assert_eq!(c.errno(), 0);
    c.restore_error(saved);
    c.restore_upsert_status(up);
    assert_eq!(c.errno(), 1064);
    assert_eq!(c.sqlstate(), "42000");
    c.set_error(2014, "HY000", b"x");
    assert_eq!((c.errno(), c.error()), (2014, b"x".to_vec()));
    c.set_error(0, "", b"");
    assert_eq!(c.errno(), 0);
    c.set_affected_rows(7);
    assert_eq!(c.affected_rows(), 7);
    // multi statements on, then off immediately
    c.set_server_option(php_mysqlnd::consts::OPTION_MULTI_STATEMENTS_ON)
        .unwrap();
    c.query(b"SELECT 1; SELECT 2").unwrap();
    let _ = c.store_result().unwrap();
    // a result is still pending: switching off fails silently, error untouched
    let _ = c.query(b"SELEC 2");
    let before = c.errno();
    assert!(!c.exit_multi_statements());
    assert_eq!(c.errno(), before);
    while c.next_result().unwrap() {
        let _ = c.store_result().unwrap();
    }
    assert!(c.exit_multi_statements());
    assert_eq!(c.query(b"SELECT 1; SELECT 2").unwrap_err().errno, 1064);
}

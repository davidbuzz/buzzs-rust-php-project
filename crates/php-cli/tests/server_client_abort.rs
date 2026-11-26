// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A client that goes away ends the script it asked for
//! (`php_handle_aborted_connection()`): the write to it fails, the
//! connection status becomes aborted, and unless `ignore_user_abort` is on
//! the script is stopped as `zend_bailout()` stops it, its shutdown
//! functions still run. The built-in server runs one request at a time: a
//! script left writing to nobody kept it from answering anything else (a
//! test suite with a "slow endpoint" hung on it). Expected values from
//! php8.5.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    port: u16,
    root: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn start(files: &[(&str, &str)]) -> Server {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("server-client-abort-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for (name, source) in files {
        std::fs::write(root.join(name), source).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-t"])
        .arg(&root)
        .args(["-S", "127.0.0.1:0"])
        .current_dir(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn php -S");
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let banner = lines.next().unwrap().unwrap();
    let port: u16 = banner
        .rsplit(':')
        .next()
        .and_then(|s| s.strip_suffix(") started"))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad banner {banner}"));
    std::thread::spawn(move || for _ in lines {});
    Server { child, port, root }
}

/// Sends a GET for `path` and returns the open connection.
fn get(port: u16, path: &str) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    s
}

/// The body of the whole response to a GET for `path`.
fn body(port: u16, path: &str) -> String {
    let mut out = Vec::new();
    get(port, path).read_to_end(&mut out).expect("response");
    let text = String::from_utf8_lossy(&out).into_owned();
    text.split_once("\r\n\r\n")
        .map_or(text.clone(), |(_, b)| b.to_string())
}

/// Reads the start of the response, then closes the connection.
fn abort_after_first_bytes(port: u16, path: &str) {
    let mut s = get(port, path);
    let mut first = [0u8; 64];
    let _ = s.read(&mut first).expect("the response has begun");
    drop(s);
}

fn wait_for(file: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(file) {
            if !text.is_empty() {
                return text;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{} was not written",
            file.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_client_that_goes_away_ends_its_script() {
    let srv = start(&[
        (
            "loop.php",
            "<?php
register_shutdown_function(function () {
    file_put_contents(__DIR__ . '/shutdown.txt', connection_status() . ' ' . (int) connection_aborted());
});
while (true) { echo '<1>'; flush(); usleep(500); }
",
        ),
        (
            "kept.php",
            "<?php
ignore_user_abort(true);
for ($i = 0; $i < 5000; $i++) {
    echo '<1>'; flush(); usleep(1000);
    if (connection_aborted()) { file_put_contents(__DIR__ . '/kept.txt', connection_status()); break; }
}
",
        ),
        ("ok.php", "<?php echo 'ok';"),
    ]);
    // The endless script ends when its client has gone, its shutdown
    // function sees the aborted connection, and the server answers again.
    abort_after_first_bytes(srv.port, "/loop.php");
    assert_eq!(wait_for(&srv.root.join("shutdown.txt")), "1 1");
    assert_eq!(body(srv.port, "/ok.php"), "ok");
    // With `ignore_user_abort` the script goes on and can ask.
    abort_after_first_bytes(srv.port, "/kept.php");
    assert_eq!(wait_for(&srv.root.join("kept.txt")), "1");
    assert_eq!(body(srv.port, "/ok.php"), "ok");
}

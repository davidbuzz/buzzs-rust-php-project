// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `php-cgi` as a plain CGI program, one process per request, with the CGI
//! environment (ref: `sapi/cgi/cgi_main.c`). Expected outputs are
//! php-cgi8.5's for the same script and environment; `X-Powered-By` is
//! left out of the comparisons (its version differs).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const PHP_CGI: &str = env!("CARGO_BIN_EXE_php-cgi");

/// A scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("phprust-cgi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `script` as a GET request through `php-cgi -n <args>`; the output
/// without its `X-Powered-By` line.
fn cgi(tag: &str, script: &str, args: &[&str], method: &str, body: &[u8]) -> String {
    let dir = TempDir::new(tag);
    let file = dir.0.join("s.php");
    std::fs::write(&file, script).unwrap();
    let mut child = Command::new(PHP_CGI)
        .arg("-n")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("REQUEST_METHOD", method)
        .env("SCRIPT_FILENAME", &file)
        .env("REDIRECT_STATUS", "200")
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("CONTENT_TYPE", "application/x-www-form-urlencoded")
        .current_dir(&dir.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn php-cgi");
    child.stdin.take().unwrap().write_all(body).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_inclusive('\n')
        .filter(|l| !l.starts_with("X-Powered-By:"))
        .collect()
}

/// `flush()` under plain CGI is `sapi_cgi_flush()`: `fflush(stdout)` only.
/// With the output still buffered (`ob_start()`, `output_buffering`) the
/// headers are not sent, and a later `header()` still reaches them.
#[test]
fn flush_does_not_send_the_headers() {
    let script =
        "<?php\nob_start();\necho \"a\";\nflush();\nheader(\"X-After-Flush: 1\");\necho \"b\";\n";
    let expected = "X-After-Flush: 1\r\nContent-type: text/html; charset=UTF-8\r\n\r\nab";
    assert_eq!(cgi("flush", script, &[], "GET", b""), expected);
    let script = "<?php\necho \"a\";\nflush();\nheader(\"X-After-Flush: 1\");\necho \"b\";\n";
    assert_eq!(
        cgi(
            "flushob",
            script,
            &["-d", "output_buffering=4096"],
            "GET",
            b""
        ),
        expected
    );
}

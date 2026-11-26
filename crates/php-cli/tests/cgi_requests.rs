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

/// Runs `script` as a request through `php-cgi -n <args>`; the output
/// without its `X-Powered-By` line.
fn cgi(tag: &str, script: &str, args: &[&str], method: &str, body: &[u8]) -> String {
    cgi_raw(tag, script, args, method, body)
        .split_inclusive('\n')
        .filter(|l| !l.starts_with("X-Powered-By:"))
        .collect()
}

/// [`cgi`] with the whole output.
fn cgi_raw(tag: &str, script: &str, args: &[&str], method: &str, body: &[u8]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
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

/// An `E_CORE_ERROR` has no location (`get_filename_lineno()`): the one
/// `ini_set('zlib.output_compression', '1')` raises next to an
/// `output_handler` is reported "in Unknown on line 0", with the stack
/// trace still naming the script's line.
#[test]
fn core_error_from_ini_set_has_no_location() {
    let out = cgi(
        "coreerr",
        "<?php\nini_set(\"zlib.output_compression\", \"1\");\necho \"x\";\n",
        &["-d", "output_handler=ob_gzhandler", "-d", "html_errors=0"],
        "GET",
        b"",
    );
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    assert_eq!(
        head,
        "Vary: Accept-Encoding\r\nContent-type: text/html; charset=UTF-8"
    );
    assert!(
        body.starts_with("\nFatal error: ini_set(): Cannot use both zlib.output_compression and output_handler together!! in Unknown on line 0\nStack trace:\n#0 "),
        "{body}"
    );
    assert!(
        body.ends_with("s.php(2): ini_set('zlib.output_com...', '1')\n#1 {main}\n"),
        "{body}"
    );
}

/// A POST body over `post_max_size` is refused at SAPI activation
/// (`sapi_read_post_data()`), before `php_request_startup()` adds
/// `X-Powered-By`: the warning sends the headers without it. `php://input`
/// still has the body.
#[test]
fn post_max_size_warning_precedes_x_powered_by() {
    let mut body = b"a=".to_vec();
    body.extend(std::iter::repeat_n(b'x', 3000));
    let out = cgi_raw(
        "postmax",
        "<?php var_dump($_POST, strlen(file_get_contents(\"php://input\")));\n",
        &["-d", "post_max_size=2K"],
        "POST",
        &body,
    );
    assert_eq!(
        out,
        "Content-type: text/html; charset=UTF-8\r\n\r\n<br />\n<b>Warning</b>:  PHP Request Startup: POST Content-Length of 3002 bytes exceeds the limit of 2048 bytes in <b>Unknown</b> on line <b>0</b><br />\narray(0) {\n}\nint(3002)\n"
    );
    let out = cgi_raw("postok", "<?php echo 1;", &[], "POST", b"a=1");
    assert!(out.starts_with("X-Powered-By: PHP/"), "{out}");
}

/// `php-cgi sub/x.php` from the command line, a relative name: the script
/// is named by its opened path (`php_fopen_primary_script()`, whose
/// `expand_filepath()` makes it absolute before the change into the
/// script's directory). `__DIR__` was "sub" inside ".../sub", so
/// `include __DIR__ . '/y.php'` looked for "sub/y.php" there and failed.
/// Expected output from php-cgi8.5.
#[test]
fn a_relative_script_name_is_the_absolute_opened_path() {
    let dir = TempDir::new("relname");
    let base = dir.0.canonicalize().unwrap();
    std::fs::create_dir_all(base.join("sub")).unwrap();
    std::fs::write(
        base.join("sub/x.php"),
        "<?php\necho __FILE__, \"\\n\", __DIR__, \"\\n\", getcwd(), \"\\n\", \
         implode(',', get_included_files()), \"\\n\";\n\
         echo include __DIR__ . '/y.php';\necho $u;\n",
    )
    .unwrap();
    std::fs::write(base.join("sub/y.php"), "<?php return \"y\\n\";").unwrap();
    let out = Command::new(PHP_CGI)
        .args(["-n", "-q", "-d", "html_errors=0", "sub/x.php"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&base)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    let b = base.display();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "{b}/sub/x.php\n{b}/sub\n{b}/sub\n{b}/sub/x.php\ny\n\
             \nWarning: Undefined variable $u in {b}/sub/x.php on line 4\n"
        )
    );
}

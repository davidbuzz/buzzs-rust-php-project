// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `php-cgi` as a `FastCGI` server (ref: `main/fastcgi.c`, `sapi/cgi/cgi_main.c`).
//!
//! `php_cgi_speaks_fastcgi` talks the protocol to `php-cgi -b` directly.
//! `nginx_runs_php_through_fastcgi` is the end-to-end test: a real nginx
//! (its own configuration, a free port on 127.0.0.1) passes requests for
//! `*.php` to `php-cgi -b <unix socket>`, and the PHP pages are fetched
//! over HTTP with curl. It reports itself skipped when `/usr/sbin/nginx`
//! (or `$NGINX`) or curl is missing.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PHP_CGI: &str = env!("CARGO_BIN_EXE_php-cgi");

/// A scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("phprust-{tag}-{}", std::process::id()));
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

/// A child process, killed on drop.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A port nothing listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Waits until `ready()` or `child` exits; the failure says why.
fn wait_for(child: &mut Child, what: &str, ready: impl Fn() -> bool) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if ready() {
            return Ok(());
        }
        if let Ok(Some(st)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(format!("{what} exited ({st}): {err}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{what} did not start"))
}

/// The version `php-cgi -v` reports.
fn php_version() -> String {
    let out = Command::new(PHP_CGI).args(["-n", "-v"]).output().unwrap();
    let banner = String::from_utf8_lossy(&out.stdout).into_owned();
    banner
        .strip_prefix("PHP ")
        .and_then(|r| r.split(' ').next())
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------
// A minimal FastCGI client.

const BEGIN_REQUEST: u8 = 1;
const END_REQUEST: u8 = 3;
const PARAMS: u8 = 4;
const STDIN: u8 = 5;
const STDOUT: u8 = 6;
const STDERR: u8 = 7;
const GET_VALUES: u8 = 9;
const GET_VALUES_RESULT: u8 = 10;

fn record(kind: u8, id: u16, content: &[u8]) -> Vec<u8> {
    let len = u16::try_from(content.len()).unwrap();
    let pad = (8 - content.len() % 8) % 8;
    let mut out = vec![1, kind];
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.push(u8::try_from(pad).unwrap());
    out.push(0);
    out.extend_from_slice(content);
    out.resize(out.len() + pad, 0);
    out
}

fn pair(out: &mut Vec<u8>, name: &str, value: &str) {
    for len in [name.len(), value.len()] {
        if len < 128 {
            out.push(u8::try_from(len).unwrap());
        } else {
            out.extend_from_slice(&(u32::try_from(len).unwrap() | 0x8000_0000).to_be_bytes());
        }
    }
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// One record from the server: (type, request id, content).
fn read_record(s: &mut impl Read) -> Option<(u8, u16, Vec<u8>)> {
    let mut h = [0u8; 8];
    s.read_exact(&mut h).ok()?;
    let len = usize::from(u16::from_be_bytes([h[4], h[5]]));
    let mut body = vec![0u8; len + usize::from(h[6])];
    s.read_exact(&mut body).ok()?;
    body.truncate(len);
    Some((h[1], u16::from_be_bytes([h[2], h[3]]), body))
}

/// The response of one request: `FCGI_STDOUT` and `FCGI_STDERR` data.
#[derive(Debug, Default)]
struct FcgiResponse {
    stdout: String,
    stderr: String,
    ended: bool,
}

impl FcgiResponse {
    fn body(&self) -> &str {
        self.stdout.split_once("\r\n\r\n").map_or("", |(_, b)| b)
    }

    fn head(&self) -> &str {
        self.stdout.split_once("\r\n\r\n").map_or("", |(h, _)| h)
    }
}

/// Sends a request (`params`, then `body` as `STDIN`) and reads records
/// until `END_REQUEST`.
fn fcgi(
    s: &mut (impl Read + Write),
    id: u16,
    keep: bool,
    params: &[(&str, &str)],
    body: &[u8],
) -> FcgiResponse {
    let mut wire = record(BEGIN_REQUEST, id, &[0, 1, u8::from(keep), 0, 0, 0, 0, 0]);
    let mut p = Vec::new();
    for (k, v) in params {
        pair(&mut p, k, v);
    }
    wire.extend(record(PARAMS, id, &p));
    wire.extend(record(PARAMS, id, b""));
    for chunk in body.chunks(1000) {
        wire.extend(record(STDIN, id, chunk));
    }
    wire.extend(record(STDIN, id, b""));
    s.write_all(&wire).unwrap();
    let mut r = FcgiResponse::default();
    while let Some((kind, rid, content)) = read_record(s) {
        assert_eq!(rid, id, "record for another request");
        match kind {
            STDOUT => r.stdout.push_str(&String::from_utf8_lossy(&content)),
            STDERR => r.stderr.push_str(&String::from_utf8_lossy(&content)),
            END_REQUEST => {
                assert_eq!(content, [0; 8], "app status 0, FCGI_REQUEST_COMPLETE");
                r.ended = true;
                break;
            }
            k => panic!("unexpected record type {k}"),
        }
    }
    r
}

const SCRIPT: &str = r#"<?php
header('X-E2E: fastcgi');
echo "sapi=", PHP_SAPI, "\n";
echo "version=", PHP_VERSION, "\n";
echo "software=", $_SERVER['SERVER_SOFTWARE'] ?? '', "\n";
echo "gateway=", $_SERVER['GATEWAY_INTERFACE'] ?? '', "\n";
echo "script=", $_SERVER['SCRIPT_NAME'] ?? '', "\n";
echo "path_info=", $_SERVER['PATH_INFO'] ?? '', "\n";
echo "php_self=", $_SERVER['PHP_SELF'], "\n";
echo "role=", $_SERVER['FCGI_ROLE'] ?? '', "\n";
echo "method=", $_SERVER['REQUEST_METHOD'] ?? '', "\n";
echo "name=", $_GET['name'] ?? '', "\n";
echo "post=", json_encode($_POST), "\n";
echo "input=", file_get_contents('php://input'), "\n";
echo "getenv=", getenv('SERVER_PROTOCOL'), "\n";
echo "pid=", getmypid(), "\n";
echo "sum=", array_sum(range(1, 10)), "\n";
error_log("e2e-log-line");
"#;

/// The value of `key=` in a page made by [`SCRIPT`].
fn field<'a>(body: &'a str, key: &str) -> &'a str {
    body.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .unwrap_or("<missing>")
}

#[test]
#[allow(clippy::too_many_lines)]
fn php_cgi_speaks_fastcgi() {
    let dir = TempDir::new("fcgi");
    let script = dir.0.join("e2e.php");
    std::fs::write(&script, SCRIPT).unwrap();
    let port = free_port();
    let mut child = Command::new(PHP_CGI)
        .args(["-n", "-b", &format!("127.0.0.1:{port}")])
        .env("PHP_FCGI_MAX_REQUESTS", "3")
        .env_remove("PHP_FCGI_CHILDREN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let addr = format!("127.0.0.1:{port}");
    wait_for(&mut child, "php-cgi -b", || {
        TcpStream::connect(&addr).is_ok()
    })
    .unwrap();
    let mut proc = Proc(child);
    let script = script.to_str().unwrap();
    let base = [
        ("GATEWAY_INTERFACE", "CGI/1.1"),
        ("SERVER_SOFTWARE", "fcgi-test"),
        ("SERVER_PROTOCOL", "HTTP/1.1"),
        ("SCRIPT_FILENAME", script),
        ("SCRIPT_NAME", "/e2e.php"),
        ("REQUEST_URI", "/e2e.php?name=direct"),
    ];

    // Two requests on one kept connection: the same process serves both.
    let mut s = TcpStream::connect(&addr).unwrap();
    let mut params = base.to_vec();
    params.extend([("REQUEST_METHOD", "GET"), ("QUERY_STRING", "name=direct")]);
    let r1 = fcgi(&mut s, 1, true, &params, b"");
    assert!(r1.ended, "{r1:?}");
    assert!(r1.head().contains("X-E2E: fastcgi"), "{r1:?}");
    assert!(
        r1.head().contains("Content-type: text/html; charset=UTF-8"),
        "{r1:?}"
    );
    let b1 = r1.body();
    assert_eq!(field(b1, "sapi"), "cgi-fcgi");
    assert_eq!(field(b1, "version"), php_version());
    assert_eq!(field(b1, "name"), "direct");
    assert_eq!(field(b1, "role"), "RESPONDER");
    assert_eq!(field(b1, "getenv"), "HTTP/1.1");
    assert_eq!(field(b1, "sum"), "55");
    assert_eq!(
        r1.stderr, "e2e-log-line\n",
        "error_log() goes out as FCGI_STDERR"
    );

    let body = b"a=1&b=two";
    let mut params = base.to_vec();
    params.extend([
        ("REQUEST_METHOD", "POST"),
        ("CONTENT_TYPE", "application/x-www-form-urlencoded"),
        ("CONTENT_LENGTH", "9"),
    ]);
    let r2 = fcgi(&mut s, 2, true, &params, body);
    let b2 = r2.body();
    assert_eq!(field(b2, "method"), "POST");
    assert_eq!(field(b2, "post"), r#"{"a":"1","b":"two"}"#);
    assert_eq!(field(b2, "input"), "a=1&b=two");
    assert_eq!(field(b2, "pid"), field(b1, "pid"), "one FastCGI process");
    // One connection at a time (as in php-src): the kept connection is
    // served until the web server closes it.
    drop(s);

    // Management record on a new connection.
    let mut m = TcpStream::connect(&addr).unwrap();
    let mut q = Vec::new();
    pair(&mut q, "FCGI_MAX_CONNS", "");
    pair(&mut q, "FCGI_MAX_REQS", "");
    pair(&mut q, "FCGI_MPXS_CONNS", "");
    m.write_all(&record(GET_VALUES, 0, &q)).unwrap();
    let (kind, _, content) = read_record(&mut m).unwrap();
    assert_eq!(kind, GET_VALUES_RESULT);
    let mut want = Vec::new();
    pair(&mut want, "FCGI_MPXS_CONNS", "0");
    pair(&mut want, "FCGI_MAX_REQS", "1");
    pair(&mut want, "FCGI_MAX_CONNS", "1");
    assert_eq!(content, want);
    drop(m);

    // The third request (on a connection that is not kept) is the last:
    // the server closes it, then exits with status 0.
    let mut s3 = TcpStream::connect(&addr).unwrap();
    let r3 = fcgi(&mut s3, 7, false, &params, body);
    assert!(r3.ended);
    let mut rest = Vec::new();
    let _ = s3.read_to_end(&mut rest);
    assert!(rest.is_empty(), "nothing after END_REQUEST");
    // The server drains the connection until the client closes it.
    drop(s3);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = proc.0.try_wait().unwrap() {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "php-cgi did not exit after 3 requests"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
}

// ---------------------------------------------------------------------
// nginx end to end.

/// The standard `fastcgi_params` of nginx, plus what PHP needs.
const FASTCGI_PARAMS: &str = "
      fastcgi_param QUERY_STRING $query_string;
      fastcgi_param REQUEST_METHOD $request_method;
      fastcgi_param CONTENT_TYPE $content_type;
      fastcgi_param CONTENT_LENGTH $content_length;
      fastcgi_param SCRIPT_NAME $fastcgi_script_name;
      fastcgi_param REQUEST_URI $request_uri;
      fastcgi_param DOCUMENT_URI $document_uri;
      fastcgi_param DOCUMENT_ROOT $document_root;
      fastcgi_param SERVER_PROTOCOL $server_protocol;
      fastcgi_param REQUEST_SCHEME $scheme;
      fastcgi_param GATEWAY_INTERFACE CGI/1.1;
      fastcgi_param SERVER_SOFTWARE nginx/$nginx_version;
      fastcgi_param REMOTE_ADDR $remote_addr;
      fastcgi_param REMOTE_PORT $remote_port;
      fastcgi_param SERVER_ADDR $server_addr;
      fastcgi_param SERVER_PORT $server_port;
      fastcgi_param SERVER_NAME $server_name;
      fastcgi_param REDIRECT_STATUS 200;
      fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
      fastcgi_param PATH_INFO $fastcgi_path_info;
";

fn nginx_binary() -> Option<PathBuf> {
    std::env::var_os("NGINX")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("/usr/sbin/nginx")))
        .filter(|p| p.is_file())
}

fn have_curl() -> bool {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// An HTTP response as curl got it.
#[derive(Debug, Default)]
struct HttpResponse {
    status: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map_or("", |(_, v)| v.as_str())
    }
}

/// `curl -i` of `url` (with `extra` arguments).
fn curl(url: &str, extra: &[&str]) -> HttpResponse {
    let out = Command::new("curl")
        .args(["-sS", "-i", "--max-time", "20"])
        .args(extra)
        .arg(url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "curl {url}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let mut lines = head.lines();
    HttpResponse {
        status: lines.next().unwrap_or_default().to_string(),
        headers: lines
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.to_string(),
    }
}

fn nginx_conf(root: &Path, port: u16, socket: &Path) -> String {
    let r = root.display();
    format!(
        "daemon off;
master_process off;
worker_processes 1;
pid {r}/nginx.pid;
error_log {r}/error.log info;
events {{ worker_connections 64; }}
http {{
  access_log {r}/access.log;
  client_body_temp_path {r}/tmp/body;
  fastcgi_temp_path {r}/tmp/fastcgi;
  proxy_temp_path {r}/tmp/proxy;
  uwsgi_temp_path {r}/tmp/uwsgi;
  scgi_temp_path {r}/tmp/scgi;
  server {{
    listen 127.0.0.1:{port};
    server_name localhost;
    root {r}/docroot;
    location ~ [^/]\\.php(/|$) {{
      fastcgi_split_path_info ^(.+?\\.php)(/.*)$;
{FASTCGI_PARAMS}
      fastcgi_pass unix:{socket};
    }}
  }}
}}
",
        socket = socket.display(),
    )
}

#[test]
#[allow(clippy::too_many_lines)]
fn nginx_runs_php_through_fastcgi() {
    let Some(nginx) = nginx_binary() else {
        eprintln!("SKIPPED nginx_runs_php_through_fastcgi: no /usr/sbin/nginx (set NGINX)");
        return;
    };
    if !have_curl() {
        eprintln!("SKIPPED nginx_runs_php_through_fastcgi: curl is not installed");
        return;
    }
    let dir = TempDir::new("nginx");
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("docroot")).unwrap();
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    std::fs::write(root.join("docroot/e2e.php"), SCRIPT).unwrap();
    let socket = root.join("php.sock");
    let port = free_port();
    let conf = root.join("nginx.conf");
    std::fs::write(&conf, nginx_conf(&root, port, &socket)).unwrap();
    let nginx_args = |extra: &[&str]| {
        let mut a: Vec<String> = vec![
            "-p".into(),
            format!("{}/", root.display()),
            "-c".into(),
            conf.display().to_string(),
            "-e".into(),
            root.join("error.log").display().to_string(),
        ];
        a.extend(extra.iter().map(|s| (*s).to_string()));
        a
    };

    // nginx accepts the configuration that routes *.php to FastCGI.
    let t = Command::new(&nginx)
        .args(nginx_args(&["-t"]))
        .output()
        .unwrap();
    let t_err = String::from_utf8_lossy(&t.stderr).into_owned();
    assert!(
        t.status.success()
            && t_err.contains("syntax is ok")
            && t_err.contains("test is successful"),
        "nginx -t: {t_err}"
    );

    // php-cgi as the FastCGI server on a Unix socket.
    let mut php = Command::new(PHP_CGI)
        .args(["-n", "-b", socket.to_str().unwrap()])
        .env_remove("PHP_FCGI_CHILDREN")
        .env_remove("PHP_FCGI_MAX_REQUESTS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&mut php, "php-cgi -b", || {
        UnixStream::connect(&socket).is_ok()
    })
    .unwrap();
    let _php = Proc(php);

    let mut web = Command::new(&nginx)
        .args(nginx_args(&[]))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let addr = format!("127.0.0.1:{port}");
    if let Err(e) = wait_for(&mut web, "nginx", || TcpStream::connect(&addr).is_ok()) {
        let log = std::fs::read_to_string(root.join("error.log")).unwrap_or_default();
        panic!("{e}\n{log}");
    }
    let _web = Proc(web);
    let v = php_version();

    // A GET through nginx, with PATH_INFO and a query string.
    let url = format!("http://127.0.0.1:{port}/e2e.php/extra/path?name=world");
    let r = curl(&url, &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert!(r.header("Server").starts_with("nginx/"), "{r:?}");
    assert_eq!(r.header("X-Powered-By"), format!("PHP/{v}"), "{r:?}");
    assert_eq!(r.header("X-E2E"), "fastcgi", "{r:?}");
    assert_eq!(
        r.header("Content-Type"),
        "text/html; charset=UTF-8",
        "{r:?}"
    );
    let b = &r.body;
    assert_eq!(field(b, "sapi"), "cgi-fcgi", "{b}");
    assert_eq!(field(b, "version"), v, "{b}");
    assert!(field(b, "software").starts_with("nginx/"), "{b}");
    assert_eq!(field(b, "gateway"), "CGI/1.1", "{b}");
    assert_eq!(field(b, "script"), "/e2e.php", "{b}");
    assert_eq!(field(b, "path_info"), "/extra/path", "{b}");
    assert_eq!(field(b, "php_self"), "/e2e.php/extra/path", "{b}");
    assert_eq!(field(b, "role"), "RESPONDER", "{b}");
    assert_eq!(field(b, "method"), "GET", "{b}");
    assert_eq!(field(b, "name"), "world", "{b}");
    assert_eq!(field(b, "getenv"), "HTTP/1.1", "{b}");
    assert_eq!(field(b, "sum"), "55", "{b}");
    let pid = field(b, "pid").to_string();

    // A form POST: the body arrives as FCGI_STDIN.
    let post_url = format!("http://127.0.0.1:{port}/e2e.php");
    let r = curl(&post_url, &["--data", "a=1&b=two"]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert_eq!(field(&r.body, "method"), "POST", "{r:?}");
    assert_eq!(field(&r.body, "post"), r#"{"a":"1","b":"two"}"#, "{r:?}");
    assert_eq!(field(&r.body, "input"), "a=1&b=two", "{r:?}");
    assert_eq!(field(&r.body, "pid"), pid, "one long-lived FastCGI process");

    // A missing script: php-cgi's 404 page.
    let r = curl(&format!("http://127.0.0.1:{port}/missing.php"), &[]);
    assert_eq!(r.status, "HTTP/1.1 404 Not Found", "{r:?}");
    assert_eq!(r.body, "No input file specified.\n", "{r:?}");

    // error_log() reached nginx's error log as FastCGI stderr.
    let log = std::fs::read_to_string(root.join("error.log")).unwrap_or_default();
    assert!(
        log.contains("FastCGI sent in stderr: \"e2e-log-line\""),
        "nginx error log:\n{log}"
    );
    eprintln!(
        "nginx {} -> FastCGI unix:{} -> php-cgi PHP {v} (cgi-fcgi), pid {pid}: OK",
        r.header("Server"),
        socket.display()
    );
}

/// What one request changes that C resets at its end
/// (`PHP_RINIT/RSHUTDOWN_FUNCTION(basic)`) must not reach the next request
/// the same php-cgi process serves (CVE-2026-109619): a default stream
/// context, the umask.
#[test]
fn request_state_does_not_reach_the_next_request() {
    let dir = TempDir::new("fcgi-state");
    let script = dir.0.join("state.php");
    std::fs::write(
        &script,
        "<?php if (isset($_GET['set'])) { stream_context_set_default(['http' => ['user_agent' => 'leaky']]); umask(077); echo 'set'; } else { echo json_encode(stream_context_get_options(stream_context_get_default())), ' ', decoct(umask()); }",
    )
    .unwrap();
    // A socket of its own, not a TCP port: a port found free can be taken
    // by another test's server before this one binds it (the request then
    // reached a server that was shut down under it, and came back empty).
    let socket = dir.0.join("cgi.sock");
    let mut child = Command::new(PHP_CGI)
        .args(["-n", "-b"])
        .arg(&socket)
        .env_remove("PHP_FCGI_CHILDREN")
        .env_remove("PHP_FCGI_MAX_REQUESTS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&mut child, "php-cgi -b", || {
        UnixStream::connect(&socket).is_ok()
    })
    .unwrap();
    let _proc = Proc(child);
    let script = script.to_str().unwrap();
    let request = |query: &str| {
        let mut s = UnixStream::connect(&socket).unwrap();
        let params = [
            ("SCRIPT_FILENAME", script),
            ("REQUEST_METHOD", "GET"),
            ("QUERY_STRING", query),
        ];
        let r = fcgi(&mut s, 1, false, &params, b"");
        assert!(r.ended, "{r:?}");
        r.stdout
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default()
    };
    let before = request("");
    assert!(before.starts_with("[] "), "{before}");
    assert_eq!(request("set=1"), "set");
    assert_eq!(
        request(""),
        before,
        "the second request saw the first one's state"
    );
}

/// A page that keeps writing after the client left: the log shows how far
/// it got, what the shutdown function saw, and that destructors ran.
const ABORT_SCRIPT: &str = r#"<?php
$log = $_SERVER['ABORT_LOG'];
if (isset($_GET['ignore'])) {
    ignore_user_abort(true);
}
class D { function __destruct() { file_put_contents($GLOBALS['log'], 'dtor;', FILE_APPEND); } }
$d = new D;
register_shutdown_function(function () use ($log) {
    file_put_contents($log, 'shutdown:' . connection_status() . ':' . var_export(connection_aborted(), true) . ';', FILE_APPEND);
});
for ($i = 0; $i < 200; $i++) {
    echo str_repeat('x', 65536);
    flush();
    usleep(10000);
}
file_put_contents($log, 'completed;', FILE_APPEND);
"#;

/// Requests [`ABORT_SCRIPT`] from `php-cgi -b`, reads the first output
/// record, closes the connection and returns the script's log once the
/// shutdown function wrote it.
fn abort_after_first_output(query: &str) -> String {
    let dir = TempDir::new(&format!("fcgi-abort-{}", query.len()));
    let script = dir.0.join("abort.php");
    std::fs::write(&script, ABORT_SCRIPT).unwrap();
    let log = dir.0.join("abort.log");
    let port = free_port();
    let mut child = Command::new(PHP_CGI)
        .args(["-n", "-b", &format!("127.0.0.1:{port}")])
        .env_remove("PHP_FCGI_CHILDREN")
        .env_remove("PHP_FCGI_MAX_REQUESTS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let addr = format!("127.0.0.1:{port}");
    wait_for(&mut child, "php-cgi -b", || {
        TcpStream::connect(&addr).is_ok()
    })
    .unwrap();
    let _proc = Proc(child);
    let mut s = TcpStream::connect(&addr).unwrap();
    let mut p = Vec::new();
    for (k, v) in [
        ("SCRIPT_FILENAME", script.to_str().unwrap()),
        ("REQUEST_METHOD", "GET"),
        ("QUERY_STRING", query),
        ("ABORT_LOG", log.to_str().unwrap()),
    ] {
        pair(&mut p, k, v);
    }
    let mut wire = record(BEGIN_REQUEST, 1, &[0, 1, 0, 0, 0, 0, 0, 0]);
    wire.extend(record(PARAMS, 1, &p));
    wire.extend(record(PARAMS, 1, b""));
    wire.extend(record(STDIN, 1, b""));
    s.write_all(&wire).unwrap();
    match read_record(&mut s) {
        Some((STDOUT, 1, _)) => {}
        other => panic!("the first output record: {other:?}"),
    }
    drop(s);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("shutdown:") && text.contains("dtor;") {
            return text;
        }
        assert!(Instant::now() < deadline, "the script didn't end: {text:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `php_handle_aborted_connection()` (`sapi_fcgi_ub_write`): once a write
/// to the gone client fails, the script stops; shutdown functions and
/// destructors still run and see `PHP_CONNECTION_ABORTED`.
#[test]
fn a_gone_client_stops_the_script() {
    let log = abort_after_first_output("");
    assert!(!log.contains("completed;"), "{log}");
    assert!(log.contains("shutdown:1:true;"), "{log}");
}

/// With `ignore_user_abort`, the script runs to its end; its output is
/// dropped.
#[test]
fn ignore_user_abort_keeps_the_script_running() {
    let log = abort_after_first_output("ignore=1");
    assert!(log.starts_with("completed;"), "{log}");
    assert!(log.contains("shutdown:1:true;"), "{log}");
}

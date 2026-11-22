// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! nginx in front of `php-fpm`, end to end (ref: `sapi/fpm`).
//!
//! A real nginx (its own configuration, a free port on 127.0.0.1) passes
//! requests for `*.php` to a `php-fpm` master with a static pool of two
//! workers on a Unix socket; the pages are fetched over HTTP with curl.
//! The test covers a GET with `PATH_INFO`, a POST body, the pool's
//! `php_admin_value` and a request's `PHP_VALUE`, the ping and status
//! pages (the HTML status page escapes what clients sent), the
//! "File not found." page, `security.limit_extensions`, `error_log()`
//! and "Primary script unknown" reaching nginx's error log, a worker's
//! output relayed into FPM's log, the access log, the workers being
//! replaced after `pm.max_requests`, a reload (SIGUSR2) and the graceful
//! shutdown (SIGQUIT). It reports itself skipped when `/usr/sbin/nginx`
//! (or `$NGINX`) or curl is missing.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PHP_FPM: &str = env!("CARGO_BIN_EXE_php-fpm");

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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Polls `ready()` for up to 15 seconds; fails early if `child` exits.
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
    Err(format!("{what} did not get ready"))
}

/// Polls until `file` contains `needle` (15 seconds at most).
fn wait_for_text(file: &Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if text.contains(needle) || Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

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

fn curl(url: &str, extra: &[&str]) -> HttpResponse {
    let out = Command::new("curl")
        .args(["-sS", "-i", "--max-time", "20", "--path-as-is"])
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

/// The page every PHP request runs.
const SCRIPT: &str = r#"<?php
header('X-E2E: fpm');
echo "sapi=", PHP_SAPI, "\n";
echo "version=", PHP_VERSION, "\n";
echo "software=", $_SERVER['SERVER_SOFTWARE'] ?? '', "\n";
echo "script=", $_SERVER['SCRIPT_NAME'] ?? '', "\n";
echo "path_info=", $_SERVER['PATH_INFO'] ?? '', "\n";
echo "php_self=", $_SERVER['PHP_SELF'], "\n";
echo "method=", $_SERVER['REQUEST_METHOD'] ?? '', "\n";
echo "name=", $_GET['name'] ?? '', "\n";
echo "post=", json_encode($_POST), "\n";
echo "input=", file_get_contents('php://input'), "\n";
echo "pid=", getmypid(), "\n";
echo "admin=", ini_get('precision'), "\n";
echo "value=", ini_get('serialize_precision'), "\n";
echo "pool_env=", getenv('E2E_POOL') ?: '', "\n";
echo "sum=", array_sum(range(1, 10)), "\n";
error_log("e2e-log-line");
file_put_contents('php://stderr', "e2e-said-this\n");
"#;

fn field<'a>(body: &'a str, key: &str) -> &'a str {
    body.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .unwrap_or("<missing>")
}

/// The standard `fastcgi_params` of nginx.
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

/// nginx: `*.php` (and the status and ping pages) to FPM's socket.
fn nginx_conf(root: &Path, port: u16, socket: &Path) -> String {
    let r = root.display();
    let s = socket.display();
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
    location ~ ^/(fpm-status|fpm-ping)$ {{
{FASTCGI_PARAMS}
      fastcgi_pass unix:{s};
    }}
    location ~ [^/]\\.(php|txt)(/|$) {{
      fastcgi_split_path_info ^(.+?\\.(?:php|txt))(/.*)$;
{FASTCGI_PARAMS}
      fastcgi_param PHP_VALUE \"serialize_precision=5\";
      fastcgi_pass unix:{s};
    }}
  }}
}}
"
    )
}

/// FPM: a static pool of two workers, each replaced after 4 requests.
fn fpm_conf(root: &Path, socket: &Path) -> String {
    let r = root.display();
    format!(
        "[global]
pid = {r}/fpm.pid
error_log = {r}/fpm.log
log_level = notice
[e2e]
listen = {socket}
pm = static
pm.max_children = 2
pm.max_requests = 4
pm.status_path = /fpm-status
ping.path = /fpm-ping
ping.response = e2e-pong
access.log = {r}/fpm-access.log
slowlog = {r}/fpm-slow.log
request_slowlog_timeout = 1
access.format = \"%m %r%Q%q %s %{{REQUEST_METHOD}}e\"
catch_workers_output = yes
decorate_workers_output = yes
clear_env = yes
env[E2E_POOL] = $pool
php_admin_value[precision] = 7
security.limit_extensions = .php
",
        socket = socket.display(),
    )
}

fn signal(pid: u32, sig: &str) {
    let ok = Command::new("kill")
        .args([format!("-{sig}"), pid.to_string()])
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "kill -{sig} {pid}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn nginx_runs_php_through_php_fpm() {
    let Some(nginx) = nginx_binary() else {
        eprintln!("SKIPPED nginx_runs_php_through_php_fpm: no /usr/sbin/nginx (set NGINX)");
        return;
    };
    if !have_curl() {
        eprintln!("SKIPPED nginx_runs_php_through_php_fpm: curl is not installed");
        return;
    }
    let dir = TempDir::new("nginx-fpm");
    let root = dir.0.clone();
    std::fs::create_dir_all(root.join("docroot")).unwrap();
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    std::fs::write(root.join("docroot/e2e.php"), SCRIPT).unwrap();
    std::fs::write(root.join("docroot/notes.txt"), "<?php echo 'ran';").unwrap();
    std::fs::write(
        root.join("docroot/mem.php"),
        "<?php ini_set('memory_limit', '16M');\n$s = str_repeat('x', 6 << 20);\necho memory_get_usage() > (6 << 20) ? \"counted\\n\" : \"not counted\\n\";\n$a = [];\nfor ($i = 0; $i < 64; $i++) { $a[] = str_repeat('y', 1 << 20); }\necho \"survived\\n\";\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docroot/slow.php"),
        "<?php function wait() { usleep(2500000); }\nwait();\necho 'slow';",
    )
    .unwrap();
    let socket = root.join("fpm.sock");
    let port = free_port();
    let nginx_conf_path = root.join("nginx.conf");
    std::fs::write(&nginx_conf_path, nginx_conf(&root, port, &socket)).unwrap();
    let fpm_conf_path = root.join("php-fpm.conf");
    std::fs::write(&fpm_conf_path, fpm_conf(&root, &socket)).unwrap();
    let fpm_log = root.join("fpm.log");

    // php-fpm checks its configuration (-t), then runs in the foreground.
    let t = Command::new(PHP_FPM)
        .args(["-n", "-t", "-y"])
        .arg(&fpm_conf_path)
        .output()
        .unwrap();
    let t_out = String::from_utf8_lossy(&t.stderr).into_owned();
    assert!(
        t.status.success() && t_out.contains("test is successful"),
        "php-fpm -t: {t_out}"
    );
    let mut fpm = Command::new(PHP_FPM)
        .args(["-n", "-F", "-y"])
        .arg(&fpm_conf_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&mut fpm, "php-fpm", || UnixStream::connect(&socket).is_ok()).unwrap();
    let fpm_pid = fpm.id();
    let mut fpm = Proc(fpm);
    let log = wait_for_text(&fpm_log, "ready to handle connections");
    assert!(
        log.contains(&format!("NOTICE: fpm is running, pid {fpm_pid}")),
        "{log}"
    );
    assert_eq!(log.matches("NOTICE: [pool e2e] child ").count(), 2, "{log}");
    let pid_file = std::fs::read_to_string(root.join("fpm.pid")).unwrap_or_default();
    assert_eq!(pid_file.trim(), fpm_pid.to_string());

    let nginx_args = |extra: &[&str]| {
        let mut a: Vec<String> = vec![
            "-p".into(),
            format!("{}/", root.display()),
            "-c".into(),
            nginx_conf_path.display().to_string(),
            "-e".into(),
            root.join("error.log").display().to_string(),
        ];
        a.extend(extra.iter().map(|s| (*s).to_string()));
        a
    };
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
    let base = format!("http://127.0.0.1:{port}");

    // A GET with PATH_INFO and a query string.
    let r = curl(&format!("{base}/e2e.php/extra/path?name=world"), &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert!(r.header("Server").starts_with("nginx/"), "{r:?}");
    assert_eq!(r.header("X-E2E"), "fpm", "{r:?}");
    assert_eq!(
        r.header("Content-Type"),
        "text/html; charset=UTF-8",
        "{r:?}"
    );
    let b = &r.body;
    let v = field(b, "version").to_string();
    assert_eq!(r.header("X-Powered-By"), format!("PHP/{v}"), "{r:?}");
    assert_eq!(field(b, "sapi"), "fpm-fcgi", "{b}");
    assert!(field(b, "software").starts_with("nginx/"), "{b}");
    assert_eq!(field(b, "script"), "/e2e.php", "{b}");
    assert_eq!(field(b, "path_info"), "/extra/path", "{b}");
    assert_eq!(field(b, "php_self"), "/e2e.php/extra/path", "{b}");
    assert_eq!(field(b, "method"), "GET", "{b}");
    assert_eq!(field(b, "name"), "world", "{b}");
    assert_eq!(field(b, "admin"), "7", "php_admin_value[precision]: {b}");
    assert_eq!(field(b, "value"), "5", "PHP_VALUE from nginx: {b}");
    assert_eq!(field(b, "pool_env"), "e2e", "env[E2E_POOL] = $pool: {b}");
    assert_eq!(field(b, "sum"), "55", "{b}");
    let first_pid = field(b, "pid").to_string();

    // A form POST: the body arrives as FCGI_STDIN.
    let r = curl(&format!("{base}/e2e.php"), &["--data", "a=1&b=two"]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert_eq!(field(&r.body, "method"), "POST", "{r:?}");
    assert_eq!(field(&r.body, "post"), r#"{"a":"1","b":"two"}"#, "{r:?}");
    assert_eq!(field(&r.body, "input"), "a=1&b=two", "{r:?}");

    // The ping page.
    let r = curl(&format!("{base}/fpm-ping"), &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert_eq!(r.body, "e2e-pong", "{r:?}");
    assert_eq!(
        r.header("Cache-Control"),
        "no-cache, no-store, must-revalidate, max-age=0"
    );

    // The status page: text, JSON with the workers, HTML escaping what a
    // client sent (a request still shows in its worker's slot).
    let r = curl(&format!("{base}/fpm-status"), &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert!(r.body.starts_with("pool:                 e2e\n"), "{r:?}");
    assert!(r.body.contains("process manager:      static\n"), "{r:?}");
    assert!(r.body.contains("total processes:      2\n"), "{r:?}");
    let mut slow = Command::new("curl")
        .args(["-sS", "--max-time", "20", "--path-as-is"])
        .arg(format!("{base}/slow.php?<b>x</b>"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && !curl(&format!("{base}/fpm-status?full"), &[])
            .body
            .contains("slow.php")
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = curl(&format!("{base}/fpm-status?full&html"), &[]);
    assert_eq!(r.header("Content-Type"), "text/html;charset=UTF-8", "{r:?}");
    assert!(r.body.contains("<td>e2e</td>"), "{r:?}");
    assert!(
        !r.body.contains("<b>x</b>"),
        "unescaped request URI: {}",
        r.body
    );
    assert!(
        r.body.contains("/slow.php?&lt;b&gt;x&lt;/b&gt;"),
        "{}",
        r.body
    );
    let _ = slow.wait();
    // The slow request was reported, and its worker wrote its backtrace.
    let log = wait_for_text(&fpm_log, "executing too slow");
    assert!(
        log.contains("/slow.php' (request: \"GET /slow.php?<b>x</b>\") executing too slow ("),
        "{log}"
    );
    let slow_log = wait_for_text(&root.join("fpm-slow.log"), "wait()");
    assert!(slow_log.contains("  [pool e2e] pid "), "{slow_log}");
    assert!(slow_log.contains("/docroot/slow.php\n"), "{slow_log}");
    assert!(slow_log.contains("] wait() "), "{slow_log}");
    let r = curl(&format!("{base}/fpm-status?json&full"), &[]);
    assert_eq!(r.header("Content-Type"), "application/json", "{r:?}");
    assert!(
        r.body
            .starts_with("{\"pool\":\"e2e\",\"process manager\":\"static\""),
        "{r:?}"
    );
    assert_eq!(r.body.matches("{\"pid\":").count(), 2, "{r:?}");

    // A missing script, and one outside security.limit_extensions.
    let r = curl(&format!("{base}/missing.php"), &[]);
    assert_eq!(r.status, "HTTP/1.1 404 Not Found", "{r:?}");
    assert_eq!(r.body, "File not found.\n", "{r:?}");
    let r = curl(&format!("{base}/notes.txt"), &[]);
    assert_eq!(r.status, "HTTP/1.1 403 Forbidden", "{r:?}");
    assert_eq!(r.body, "Access denied.\n", "{r:?}");

    // nginx logged what the workers sent as FastCGI stderr.
    let nginx_log = wait_for_text(&root.join("error.log"), "Primary script unknown");
    assert!(
        nginx_log.contains("FastCGI sent in stderr: \"PHP message: e2e-log-line\""),
        "nginx error log:\n{nginx_log}"
    );
    assert!(
        nginx_log.contains("FastCGI sent in stderr: \"Primary script unknown\""),
        "nginx error log:\n{nginx_log}"
    );

    // FPM relayed what a worker wrote to its standard error.
    let log = wait_for_text(&fpm_log, "e2e-said-this");
    assert!(
        log.contains("WARNING: [pool e2e] child ")
            && log.contains(" said into stderr: \"e2e-said-this\""),
        "{log}"
    );
    assert!(
        log.contains("has been denied (see security.limit_extensions)"),
        "{log}"
    );

    // pm.max_requests = 4: the workers that served four requests ended
    // and were replaced; requests keep working.
    let log = wait_for_text(&fpm_log, "exited with code 0");
    assert!(log.contains("exited with code 0 after"), "{log}");
    let r = curl(&format!("{base}/e2e.php"), &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert_ne!(field(&r.body, "pid"), "<missing>");
    assert_ne!(first_pid, "<missing>");

    // The access log has one line per request in access.format.
    let access = std::fs::read_to_string(root.join("fpm-access.log")).unwrap_or_default();
    assert!(
        access.contains("GET /e2e.php?name=world 200 GET\n"),
        "{access}"
    );
    assert!(access.contains("POST /e2e.php 200 POST\n"), "{access}");
    assert!(access.contains("GET /missing.php 404 GET\n"), "{access}");

    // SIGUSR2 reloads: the master runs again with the same pid.
    signal(fpm_pid, "USR2");
    let log = wait_for_text(&fpm_log, "reloading: execvp(");
    assert!(log.contains("NOTICE: Reloading in progress ..."), "{log}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut ready = 0;
    while Instant::now() < deadline {
        let log = std::fs::read_to_string(&fpm_log).unwrap_or_default();
        ready = log.matches("ready to handle connections").count();
        if ready >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        ready,
        2,
        "{}",
        std::fs::read_to_string(&fpm_log).unwrap_or_default()
    );
    let r = curl(&format!("{base}/e2e.php"), &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "after reload: {r:?}");

    // `memory_limit` holds in a worker: a 6 MiB string shows in
    // `memory_get_usage()`, and growing past the limit ends the request
    // with PHP's fatal error (php-fpm installs the counting allocator).
    let mem = curl(&format!("{base}/mem.php"), &[]);
    assert!(mem.body.starts_with("counted\n"), "{mem:?}");
    assert!(!mem.body.contains("survived"), "{mem:?}");
    let logs = format!(
        "{}{}",
        std::fs::read_to_string(root.join("error.log")).unwrap_or_default(),
        std::fs::read_to_string(&fpm_log).unwrap_or_default()
    );
    assert!(
        mem.body
            .contains("Allowed memory size of 16777216 bytes exhausted")
            || logs.contains("Allowed memory size of 16777216 bytes exhausted"),
        "{mem:?}\n{logs}"
    );

    // SIGQUIT: graceful shutdown; the socket and the pid file go.
    signal(fpm_pid, "QUIT");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut status = None;
    while Instant::now() < deadline {
        if let Ok(Some(st)) = fpm.0.try_wait() {
            status = Some(st);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(&fpm_log).unwrap_or_default();
    assert!(status.is_some_and(|s| s.success()), "{status:?}\n{log}");
    assert!(log.contains("NOTICE: Finishing ..."), "{log}");
    assert!(
        log.trim_end().ends_with("NOTICE: exiting, bye-bye!"),
        "{log}"
    );
    assert!(!socket.exists(), "the socket is removed");
    assert!(!root.join("fpm.pid").exists(), "the pid file is removed");
    eprintln!(
        "nginx {} -> FastCGI unix:{} -> php-fpm PHP {v} (fpm-fcgi), master {fpm_pid}: OK",
        r.header("Server"),
        socket.display()
    );
}

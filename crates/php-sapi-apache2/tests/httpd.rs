//! End-to-end tests: loads `libphprust_apache2.so` into the system
//! `apache2` binary (prefork MPM) on 127.0.0.1:8089 and exercises it over
//! HTTP with `curl`.
//!
//! Ignored by default (needs apache2 + curl and a free port):
//! `cargo test -p php-sapi-apache2 --test httpd -- --ignored --nocapture`.
//! Everything lives under `/tmp/claude-1000/wt-apache/it`; the server is
//! stopped (and waited for) when the test ends, even on failure.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PORT: u16 = 8089;
const MODULES: &str = "/usr/lib/apache2/modules";

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace().join("target"), PathBuf::from)
}

/// The module built by this `cargo test` run (cargo leaves the cdylib of
/// a test dependency in `deps/`; a plain `cargo build` puts it in the
/// profile directory) — the newer one wins — and the phprust CLI used for
/// output comparisons (`None` unless `cargo build -p php-cli` ran).
///
/// The test cannot invoke cargo itself: the outer `cargo test` holds the
/// build-directory lock while tests run.
fn build() -> (PathBuf, Option<PathBuf>) {
    let profile = target_dir().join("debug");
    let mtime = |p: &Path| p.metadata().and_then(|m| m.modified()).ok();
    let so = [
        profile.join("deps/libphprust_apache2.so"),
        profile.join("libphprust_apache2.so"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .max_by_key(|p| mtime(p))
    .expect("libphprust_apache2.so not built");
    let cli = profile.join("php");
    (so, cli.is_file().then_some(cli))
}

fn port_open() -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{PORT}").parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
}

/// A running httpd; stopped on drop.
struct Server {
    child: Child,
    root: PathBuf,
}

impl Server {
    fn start(root: &Path, conf: &str) -> Result<Server, String> {
        let conf_path = root.join("httpd.conf");
        std::fs::write(&conf_path, conf).unwrap();
        let _ = std::fs::write(root.join("logs/error.log"), "");
        // Own process group: on shutdown prefork signals its whole group.
        let child = Command::new("apache2")
            .process_group(0)
            .env_remove("LD_LIBRARY_PATH")
            .args(["-f", conf_path.to_str().unwrap(), "-DFOREGROUND"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("apache2: {e}"))?;
        let mut server = Server {
            child,
            root: root.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if port_open() {
                return Ok(server);
            }
            if let Ok(Some(st)) = server.child.try_wait() {
                let mut err = String::new();
                if let Some(mut e) = server.child.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                return Err(format!("apache2 exited ({st}): {err}\n{}", server.log()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("apache2 did not start\n{}", server.log()))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("logs/error.log")).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SAFETY: plain kill(2) of our own child.
        unsafe {
            libc::kill(i32::try_from(self.child.id()).unwrap(), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Children of prefork exit with the parent; wait for the port.
        let deadline = Instant::now() + Duration::from_secs(5);
        while port_open() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn config(root: &Path, so: &Path, mpm: &str, extra: &str) -> String {
    let d = root.join("docroot");
    format!(
        r#"ServerRoot "{root}"
Listen 127.0.0.1:{PORT}
PidFile {root}/logs/httpd.pid
ErrorLog {root}/logs/error.log
LogLevel warn php:info
ServerName localhost
LoadModule mpm_{mpm}_module {MODULES}/mod_mpm_{mpm}.so
LoadModule authz_core_module {MODULES}/mod_authz_core.so
LoadModule dir_module {MODULES}/mod_dir.so
LoadModule mime_module {MODULES}/mod_mime.so
LoadModule env_module {MODULES}/mod_env.so
LoadModule setenvif_module {MODULES}/mod_setenvif.so
LoadModule alias_module {MODULES}/mod_alias.so
LoadModule php_module {so}
StartServers 2
<IfModule mpm_prefork_module>
  MinSpareServers 1
  MaxSpareServers 3
</IfModule>
TypesConfig /etc/mime.types
DocumentRoot "{d}"
<Directory "{d}">
  AllowOverride All
  Require all granted
</Directory>
DirectoryIndex index.php index.html
<FilesMatch "\.php$">
  SetHandler application/x-httpd-php
</FilesMatch>
<Directory "{d}/admin">
  php_admin_value precision 3
  php_admin_flag display_errors on
</Directory>
<Directory "{d}/slow">
  php_value max_execution_time 1
</Directory>
SetEnv FROM_SETENV hello
{extra}
"#,
        root = root.display(),
        so = so.display(),
        d = d.display(),
    )
}

const SCRIPTS: &[(&str, &str)] = &[
    ("hello.php", "<?php echo 'Hello ', PHP_SAPI, ' ', php_sapi_name(), \"\\n\";\n"),
    (
        "generic.php",
        "<?php\n$a = ['x' => 1.5, 'y' => [1, 2, 'three'], 'z' => null];\nvar_dump($a);\necho json_encode($a), \"\\n\";\nprintf(\"%05.2f|%-6s|%x\\n\", 3.14159, 'ab', 255);\necho str_pad(strrev('phprust'), 12, '*', STR_PAD_BOTH), \"\\n\";\necho implode(',', array_map(fn($v) => $v ** 2, range(1, 5))), \"\\n\";\necho 0.1 + 0.2, ' ', 1e100, ' ', intdiv(7, 2), \"\\n\";\n",
    ),
    (
        "server.php",
        "<?php\n$s = $_SERVER;\nforeach (['REQUEST_TIME', 'REQUEST_TIME_FLOAT'] as $k) { echo $k, '=', gettype($s[$k]), \"\\n\"; unset($s[$k]); }\nunset($s['PATH'], $s['REMOTE_PORT']);\nksort($s);\nforeach ($s as $k => $v) echo $k, '=', $v, \"\\n\";\n",
    ),
    ("vars.php", "<?php echo json_encode([$_GET, $_POST, $_COOKIE, $_REQUEST, file_get_contents('php://input')]), \"\\n\";\n"),
    (
        "headers.php",
        "<?php\nheader('X-Test: one');\nheader('X-Test: two', false);\nheader('Content-Type: text/plain');\nsetcookie('c1', 'v a l', ['path' => '/', 'httponly' => true]);\nhttp_response_code(201);\necho implode(\"\\n\", headers_list()), \"\\n\";\nheader('X-Late: 1');\n",
    ),
    ("redirect.php", "<?php header('Location: /hello.php');\n"),
    ("notfound.php", "<?php header('HTTP/1.1 404 Not Found'); echo \"missing\\n\";\n"),
    (
        "upload.php",
        "<?php\n$f = $_FILES['f'];\necho $_POST['t'], '|', $f['name'], '|', $f['type'], '|', $f['size'], '|', $f['error'], '|', file_get_contents($f['tmp_name']), \"\\n\";\n$dest = __DIR__ . '/../moved.txt';\nvar_dump(move_uploaded_file($f['tmp_name'], $dest), file_get_contents($dest));\necho $_FILES['g']['tmp_name'][0], \"\\n\";\n",
    ),
    ("warn.php", "<?php\necho $undefined;\necho \"after\\n\";\n"),
    ("stream.php", "<?php\necho \"first\\n\"; flush();\nusleep(600000);\necho \"second\\n\";\n"),
    ("elog.php", "<?php error_log('from error_log'); echo \"ok\\n\";\n"),
    (
        "apfn.php",
        "<?php\n$h = getallheaders(); echo $h['X-Foo'], \"\\n\";\nvar_dump(apache_note('n', 'v'), apache_note('n'), apache_getenv('FROM_SETENV'));\nvar_dump(str_starts_with(apache_get_version(), 'Apache/2.4'), in_array('mod_php', apache_get_modules()));\n$o = apache_lookup_uri('/hello.php'); echo $o->status, ' ', $o->uri, ' ', $o->handler, \"\\n\";\necho \"virtual:\\n\"; var_dump(virtual('/hello.php'));\n",
    ),
    ("admin/lock.php", "<?php var_dump(ini_get('precision'), ini_set('precision', '10'), ini_get('display_errors'));\n"),
    ("htaccess/ini.php", "<?php\necho ini_get('precision'), '|', ini_get('display_errors'), \"\\n\";\necho $nope;\necho 1/3, \"\\n\";\n"),
    ("htaccess/.htaccess", "php_value precision 5\nphp_flag display_errors off\nphp_flag log_errors on\n"),
    ("htaccess/fatal.php", "<?php undefined_function();\n"),
    ("small/.htaccess", "php_value post_max_size 10\n"),
    ("small/post.php", "<?php var_dump($_POST);\n"),
    ("off/.htaccess", "php_flag engine off\n"),
    ("off/src.php", "<?php echo 'not run';\n"),
    ("slow/loop.php", "<?php echo \"start\\n\"; flush(); while (true) { $x = 1; }\n"),
];

/// A parsed HTTP response.
#[derive(Debug, Default)]
struct Resp {
    status: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

fn curl(root: &Path, args: &[&str]) -> Resp {
    let hdr = root.join("curl.headers");
    let body = root.join("curl.body");
    let _ = std::fs::remove_file(&hdr);
    let _ = std::fs::remove_file(&body);
    let out = Command::new("curl")
        .args(["-s", "--max-time", "20", "-D"])
        .arg(&hdr)
        .arg("-o")
        .arg(&body)
        .args(args)
        .output()
        .expect("curl");
    let raw = std::fs::read_to_string(&hdr).unwrap_or_default();
    // The last header block (skip `100 Continue`).
    let block = raw
        .split("\r\n\r\n")
        .filter(|b| !b.trim().is_empty())
        .last()
        .unwrap_or_default()
        .to_string();
    let mut lines = block.lines();
    let status = lines.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|l| {
            l.split_once(": ")
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    let _ = out;
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&std::fs::read(&body).unwrap_or_default()).into_owned(),
    }
}

fn url(path: &str) -> String {
    format!("http://127.0.0.1:{PORT}{path}")
}

/// Collects failures instead of stopping at the first one.
#[derive(Default)]
struct Checks(Vec<String>);

impl Checks {
    fn eq(
        &mut self,
        what: &str,
        got: &(impl std::fmt::Debug + PartialEq<String>),
        want: impl Into<String>,
    ) {
        let want = want.into();
        if *got != want {
            self.0
                .push(format!("{what}:\n  got:  {got:?}\n  want: {want:?}"));
        }
    }

    fn ok(&mut self, what: &str, cond: bool, detail: impl std::fmt::Display) {
        if !cond {
            self.0.push(format!("{what}: {detail}"));
        }
    }
}

fn setup_docroot(root: &Path) {
    let d = root.join("docroot");
    let _ = std::fs::remove_dir_all(&d);
    for (name, src) in SCRIPTS {
        let p = d.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, src).unwrap();
    }
    std::fs::create_dir_all(root.join("logs")).unwrap();
}

#[test]
#[ignore = "needs apache2, curl and port 8089"]
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
fn apache_prefork_end_to_end() {
    let (so, cli) = build();
    let root = PathBuf::from("/tmp/claude-1000/wt-apache/it");
    std::fs::create_dir_all(&root).unwrap();
    setup_docroot(&root);
    assert!(
        !port_open(),
        "port {PORT} is busy: stop the other httpd first"
    );
    let server =
        Server::start(&root, &config(&root, &so, "prefork", "")).unwrap_or_else(|e| panic!("{e}"));
    let docroot = root.join("docroot").display().to_string();
    let v = php_vm_version();
    let mut c = Checks::default();

    // Basic response and SAPI defaults.
    let r = curl(&root, &[&url("/hello.php")]);
    c.eq("hello status", &r.status.clone(), "HTTP/1.1 200 OK");
    c.eq(
        "hello body",
        &r.body.clone(),
        "Hello apache2handler apache2handler\n",
    );
    c.eq(
        "hello content-type",
        &r.header("Content-Type").unwrap_or_default().to_string(),
        "text/html; charset=UTF-8",
    );
    c.eq(
        "x-powered-by",
        &r.header("X-Powered-By").unwrap_or_default().to_string(),
        format!("PHP/{v}"),
    );
    c.ok(
        "server banner",
        r.header("Server")
            .unwrap_or_default()
            .contains(&format!("PHP/{v}")),
        r.header("Server").unwrap_or_default(),
    );
    c.eq(
        "content-length",
        &r.header("Content-Length").unwrap_or_default().to_string(),
        "36",
    );

    // Same output as the CLI for SAPI-independent code.
    let r = curl(&root, &[&url("/generic.php")]);
    if let Some(cli) = &cli {
        let cli_out = Command::new(cli)
            .arg(root.join("docroot/generic.php"))
            .output()
            .expect("php cli");
        c.eq(
            "generic vs cli",
            &r.body.clone(),
            String::from_utf8_lossy(&cli_out.stdout).into_owned(),
        );
    } else {
        eprintln!("note: target/debug/php not built; skipping the CLI comparison");
    }

    // $_SERVER.
    let r = curl(
        &root,
        &[
            &url("/server.php/extra/path?a=1&b[]=2"),
            "-H",
            "X-Custom: yes",
            "-u",
            "user:pw",
        ],
    );
    let expected_server = format!(
        "REQUEST_TIME=integer\nREQUEST_TIME_FLOAT=double\n\
CONTEXT_DOCUMENT_ROOT={docroot}\nCONTEXT_PREFIX=\nDOCUMENT_ROOT={docroot}\nFROM_SETENV=hello\nGATEWAY_INTERFACE=CGI/1.1\n\
HTTP_ACCEPT=*/*\nHTTP_HOST=127.0.0.1:{PORT}\nHTTP_USER_AGENT={ua}\nHTTP_X_CUSTOM=yes\n\
PATH_INFO=/extra/path\nPATH_TRANSLATED={docroot}/extra/path\nPHP_AUTH_PW=pw\nPHP_AUTH_USER=user\nPHP_SELF=/server.php/extra/path\n\
QUERY_STRING=a=1&b[]=2\nREMOTE_ADDR=127.0.0.1\nREQUEST_METHOD=GET\nREQUEST_SCHEME=http\nREQUEST_URI=/server.php/extra/path?a=1&b[]=2\n\
SCRIPT_FILENAME={docroot}/server.php\nSCRIPT_NAME=/server.php\nSERVER_ADDR=127.0.0.1\nSERVER_ADMIN=[no address given]\nSERVER_NAME=127.0.0.1\n\
SERVER_PORT={PORT}\nSERVER_PROTOCOL=HTTP/1.1\nSERVER_SIGNATURE=\nSERVER_SOFTWARE={server}\n",
        ua = curl_ua(),
        server = r_header_server(&root),
    );
    c.eq("$_SERVER", &r.body.clone(), expected_server);

    // Request variables.
    let r = curl(
        &root,
        &[
            &url("/vars.php?g=1&a[]=x"),
            "-d",
            "p=2&a[]=y",
            "-b",
            "k=v; k2=v%202",
        ],
    );
    c.eq("vars", &r.body.clone(),
        r#"[{"g":"1","a":["x"]},{"p":"2","a":["y"]},{"k":"v","k2":"v 2"},{"g":"1","a":["y"],"p":"2","k":"v","k2":"v 2"},"p=2&a[]=y"]"#.to_string() + "\n",
    );
    let r = curl(
        &root,
        &[
            &url("/vars.php"),
            "-X",
            "PUT",
            "--data-binary",
            "{\"raw\":true}",
            "-H",
            "Content-Type: application/json",
        ],
    );
    c.eq(
        "php://input",
        &r.body.clone(),
        "[[],[],[],[],\"{\\\"raw\\\":true}\"]\n",
    );

    // Headers, cookies, status.
    let r = curl(&root, &[&url("/headers.php")]);
    c.eq("headers status", &r.status.clone(), "HTTP/1.1 201 Created");
    c.eq(
        "x-test",
        &format!("{:?}", r.all("X-Test")),
        "[\"one\", \"two\"]",
    );
    c.eq(
        "set-cookie",
        &r.header("Set-Cookie").unwrap_or_default().to_string(),
        "c1=v%20a%20l; path=/; HttpOnly",
    );
    c.eq(
        "content-type text/plain",
        &r.header("Content-Type").unwrap_or_default().to_string(),
        "text/plain;charset=UTF-8",
    );
    c.eq("headers body", &r.body.clone(),
        format!(
            "X-Powered-By: PHP/{v}\nX-Test: one\nX-Test: two\nContent-type: text/plain;charset=UTF-8\nSet-Cookie: c1=v%20a%20l; path=/; HttpOnly\n\
<br />\n<b>Warning</b>:  Cannot modify header information - headers already sent in <b>{docroot}/headers.php</b> on line <b>8</b><br />\n"
        ),
    );
    let r = curl(&root, &[&url("/redirect.php")]);
    c.eq("redirect", &r.status.clone(), "HTTP/1.1 302 Found");
    c.eq(
        "location",
        &r.header("Location").unwrap_or_default().to_string(),
        "/hello.php",
    );
    let r = curl(&root, &[&url("/redirect.php"), "-d", "x=1"]);
    c.eq(
        "redirect after POST",
        &r.status.clone(),
        "HTTP/1.1 303 See Other",
    );
    let r = curl(&root, &[&url("/notfound.php")]);
    c.eq("404", &r.status.clone(), "HTTP/1.1 404 Not Found");
    c.eq("404 body", &r.body.clone(), "missing\n");

    // Uploads.
    let up = root.join("up.txt");
    std::fs::write(&up, "uploaded content").unwrap();
    let _ = std::fs::remove_file(root.join("moved.txt"));
    let f = format!("f=@{};type=text/plain", up.display());
    let g = format!("g[]=@{}", up.display());
    let r = curl(
        &root,
        &[&url("/upload.php"), "-F", "t=field", "-F", &f, "-F", &g],
    );
    let mut lines = r.body.lines();
    c.eq(
        "upload fields",
        &lines.next().unwrap_or_default().to_string(),
        "field|up.txt|text/plain|16|0|uploaded content",
    );
    c.eq(
        "move_uploaded_file",
        &lines.next().unwrap_or_default().to_string(),
        "bool(true)",
    );
    c.eq(
        "moved content",
        &lines.next().unwrap_or_default().to_string(),
        "string(16) \"uploaded content\"",
    );
    let tmp = lines.next().unwrap_or_default().to_string();
    c.ok(
        "tmp upload cleaned up",
        tmp.starts_with("/tmp/php") && !Path::new(&tmp).exists(),
        &tmp,
    );

    // Errors: display (HTML) vs log.
    let r = curl(&root, &[&url("/warn.php")]);
    c.eq("display_errors html", &r.body.clone(),
        format!("<br />\n<b>Warning</b>:  Undefined variable $undefined in <b>{docroot}/warn.php</b> on line <b>2</b><br />\nafter\n"),
    );
    let r = curl(&root, &[&url("/htaccess/ini.php")]);
    c.eq("htaccess php_value", &r.body.clone(), "5|0\n0.33333\n");
    let r = curl(&root, &[&url("/htaccess/fatal.php")]);
    c.eq(
        "fatal status",
        &r.status.clone(),
        "HTTP/1.0 500 Internal Server Error",
    );
    c.eq("fatal body", &r.body.clone(), "");
    let r = curl(&root, &[&url("/elog.php")]);
    c.eq("elog", &r.body.clone(), "ok\n");
    let log = server.log();
    c.ok(
        "warning logged",
        log.contains("[php:warn] [pid ")
            && log.contains(&format!(
                "PHP Warning:  Undefined variable $nope in {docroot}/htaccess/ini.php on line 3"
            )),
        &log,
    );
    c.ok(
        "fatal logged",
        log.contains("[php:error]")
            && log.contains(
                "PHP Fatal error:  Uncaught Error: Call to undefined function undefined_function()",
            ),
        &log,
    );
    c.ok(
        "error_log() logged",
        log.contains("[php:notice]") && log.contains("from error_log"),
        &log,
    );

    // php_admin_* lock-out.
    let r = curl(&root, &[&url("/admin/lock.php")]);
    c.eq(
        "admin lock",
        &r.body.clone(),
        "string(1) \"3\"\nbool(false)\nstring(1) \"1\"\n",
    );

    // post_max_size.
    let r = curl(
        &root,
        &[&url("/small/post.php"), "-d", "a=0123456789abcdef"],
    );
    c.eq("post_max_size", &r.body.clone(),
        "<br />\n<b>Warning</b>:  PHP Request Startup: POST Content-Length of 18 bytes exceeds the limit of 10 bytes in <b>Unknown</b> on line <b>0</b><br />\narray(0) {\n}\n",
    );

    // engine off: the file is served by the default handler.
    let r = curl(&root, &[&url("/off/src.php")]);
    c.eq("engine off", &r.body.clone(), "<?php echo 'not run';\n");

    // Apache functions and virtual().
    let r = curl(&root, &[&url("/apfn.php"), "-H", "X-Foo: bar"]);
    c.eq("apache functions", &r.body.clone(),
        "bar\nbool(false)\nstring(1) \"v\"\nstring(5) \"hello\"\nbool(true)\nbool(true)\n200 /hello.php application/x-httpd-php\nvirtual:\nHello apache2handler apache2handler\nbool(true)\n",
    );

    // flush() streams.
    match streamed_gap("/stream.php") {
        Ok(gap) => c.ok(
            "flush streams",
            gap >= Duration::from_millis(400),
            format!("gap {gap:?}"),
        ),
        Err(e) => c.ok("flush streams", false, e),
    }

    // max_execution_time: the child is terminated, the server recovers.
    let r = curl(&root, &[&url("/slow/loop.php")]);
    c.ok("timeout output", r.body.starts_with("start\n"), &r.body);
    std::thread::sleep(Duration::from_millis(300));
    let log = server.log();
    c.ok(
        "timeout logged",
        log.contains("PHP Fatal error:  Maximum execution time of 1 second exceeded"),
        &log,
    );
    let mut recovered = false;
    for _ in 0..50 {
        if curl(&root, &[&url("/hello.php")]).body == "Hello apache2handler apache2handler\n" {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    c.ok("server recovers after timeout", recovered, "");

    drop(server);
    assert!(
        c.0.is_empty(),
        "{} check(s) failed:\n{}",
        c.0.len(),
        c.0.join("\n")
    );
}

#[test]
#[ignore = "needs apache2, curl and port 8089"]
fn threaded_mpm_requires_opt_in() {
    let (so, _) = build();
    let root = PathBuf::from("/tmp/claude-1000/wt-apache/it-event");
    std::fs::create_dir_all(&root).unwrap();
    setup_docroot(&root);
    assert!(
        !port_open(),
        "port {PORT} is busy: stop the other httpd first"
    );
    let err = match Server::start(&root, &config(&root, &so, "event", "")) {
        Ok(_) => String::new(),
        Err(e) => e,
    };
    assert!(
        err.contains("threaded MPM") && err.contains("PhpRustAllowThreads On"),
        "event MPM must be refused: {err}"
    );
    let server = Server::start(
        &root,
        &config(&root, &so, "event", "PhpRustAllowThreads On"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                let dir = root
                    .join(format!("c{:?}", std::thread::current().id()).replace(['(', ')'], ""));
                std::fs::create_dir_all(&dir).unwrap();
                curl(&dir, &[&url("/hello.php")]).body
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), "Hello apache2handler apache2handler\n");
    }
    drop(server);
}

fn php_vm_version() -> String {
    php_vm::PHP_VERSION.to_string()
}

fn curl_ua() -> String {
    let out = Command::new("curl")
        .arg("--version")
        .output()
        .expect("curl");
    let v = String::from_utf8_lossy(&out.stdout);
    let ver = v.split_whitespace().nth(1).unwrap_or_default();
    format!("curl/{ver}")
}

fn r_header_server(root: &Path) -> String {
    curl(root, &[&url("/hello.php")])
        .header("Server")
        .unwrap_or_default()
        .to_string()
}

/// Requests `path` over a raw socket and measures the time between the
/// first and the second line of the body.
fn streamed_gap(path: &str) -> Result<Duration, String> {
    let mut s = TcpStream::connect(("127.0.0.1", PORT)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut first_at = None;
    loop {
        let n = s.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if first_at.is_none() && text.contains("first\n") {
            first_at = Some(Instant::now());
        }
        if text.contains("second\n") {
            return first_at
                .map(|t| t.elapsed())
                .ok_or_else(|| "both lines arrived together".to_string());
        }
    }
    Err(format!(
        "incomplete response: {}",
        String::from_utf8_lossy(&buf)
    ))
}

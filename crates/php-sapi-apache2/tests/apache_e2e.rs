// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End to end: `mod_php` in a real Apache httpd (ref: sapi/apache2handler).
//!
//! The module this build made (`libphprust_apache2.so`) is loaded into
//! the system `apache2` with a configuration of its own (prefork MPM, a
//! free port on 127.0.0.1, everything under a temporary directory). The
//! test checks that httpd accepts the configuration and lists
//! `php_module` among its loaded modules (`apache2 -M`), then runs PHP
//! through `http://127.0.0.1:<port>/...` URLs with curl. It reports itself
//! skipped when `/usr/sbin/apache2` (or `$APACHE2`), the prefork MPM or
//! curl is missing. The fuller behaviour checks are in `httpd.rs`.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MODULES: &str = "/usr/lib/apache2/modules";

/// The module next to this test binary (`target/<profile>/deps`, where
/// cargo leaves the cdylib of a test dependency, or the profile directory
/// of a plain build): the newer one.
fn module() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let profile = deps.parent().unwrap();
    let mtime = |p: &Path| p.metadata().and_then(|m| m.modified()).ok();
    [
        deps.join("libphprust_apache2.so"),
        profile.join("libphprust_apache2.so"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .max_by_key(|p| mtime(p))
    .expect("libphprust_apache2.so was not built")
}

fn apache2() -> Option<PathBuf> {
    std::env::var_os("APACHE2")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("/usr/sbin/apache2")))
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

/// A port nothing listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A scratch directory, removed on drop.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running httpd (its own process group), stopped on drop.
struct Httpd {
    child: Child,
}

impl Drop for Httpd {
    fn drop(&mut self) {
        // SAFETY: kill(2) of our own child; prefork then stops its
        // children.
        unsafe {
            libc::kill(i32::try_from(self.child.id()).unwrap_or(0), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn config(root: &Path, port: u16, so: &Path) -> String {
    let r = root.display();
    format!(
        r#"ServerRoot "{r}"
Listen 127.0.0.1:{port}
PidFile {r}/logs/httpd.pid
ErrorLog {r}/logs/error.log
LogLevel warn
ServerName localhost
LoadModule mpm_prefork_module {MODULES}/mod_mpm_prefork.so
LoadModule authz_core_module {MODULES}/mod_authz_core.so
LoadModule dir_module {MODULES}/mod_dir.so
LoadModule mime_module {MODULES}/mod_mime.so
LoadModule php_module {so}
StartServers 1
MinSpareServers 1
MaxSpareServers 2
TypesConfig /etc/mime.types
DocumentRoot "{r}/docroot"
<Directory "{r}/docroot">
  Require all granted
</Directory>
DirectoryIndex index.php
<FilesMatch "\.php$">
  SetHandler application/x-httpd-php
</FilesMatch>
"#,
        so = so.display(),
    )
}

const SCRIPT: &str = r#"<?php
header('X-E2E: mod_php');
echo "sapi=", PHP_SAPI, "\n";
echo "version=", PHP_VERSION, "\n";
echo "software=", $_SERVER['SERVER_SOFTWARE'] ?? '', "\n";
echo "apache=", apache_get_version(), "\n";
echo "mod_php=", var_export(in_array('mod_php', apache_get_modules()), true), "\n";
echo "script=", $_SERVER['SCRIPT_NAME'] ?? '', "\n";
echo "method=", $_SERVER['REQUEST_METHOD'] ?? '', "\n";
echo "name=", $_GET['name'] ?? '', "\n";
echo "post=", json_encode($_POST), "\n";
echo "input=", file_get_contents('php://input'), "\n";
echo "sum=", array_sum(range(1, 10)), "\n";
"#;

/// The value of `key=` in a page made by [`SCRIPT`].
fn field<'a>(body: &'a str, key: &str) -> &'a str {
    body.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .unwrap_or("<missing>")
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

#[test]
#[allow(clippy::too_many_lines)]
fn apache_loads_mod_php_and_runs_php() {
    let Some(httpd) = apache2() else {
        eprintln!("SKIPPED apache_loads_mod_php_and_runs_php: no /usr/sbin/apache2 (set APACHE2)");
        return;
    };
    if !Path::new(MODULES).join("mod_mpm_prefork.so").is_file() || !have_curl() {
        eprintln!("SKIPPED apache_loads_mod_php_and_runs_php: needs {MODULES}/mod_mpm_prefork.so and curl");
        return;
    }
    let so = module();
    let dir = TempDir(std::env::temp_dir().join(format!("phprust-apache-{}", std::process::id())));
    let root = dir.0.clone();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::create_dir_all(root.join("docroot")).unwrap();
    std::fs::write(root.join("docroot/e2e.php"), SCRIPT).unwrap();
    let port = free_port();
    let conf = root.join("httpd.conf");
    std::fs::write(&conf, config(&root, port, &so)).unwrap();
    let conf_arg = conf.to_str().unwrap();
    let ctl = |args: &[&str]| {
        let out = Command::new(&httpd)
            .env_remove("LD_LIBRARY_PATH")
            .args(["-f", conf_arg])
            .args(args)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    };

    // httpd accepts the configuration and reports the module as loaded.
    let (ok, text) = ctl(&["-t"]);
    assert!(ok && text.contains("Syntax OK"), "apache2 -t:\n{text}");
    let (ok, text) = ctl(&["-M"]);
    assert!(
        ok && text.contains("Loaded Modules:") && text.contains(" php_module (shared)"),
        "apache2 -M:\n{text}"
    );

    // Own process group: on shutdown prefork signals its whole group.
    let mut child = Command::new(&httpd)
        .process_group(0)
        .env_remove("LD_LIBRARY_PATH")
        .args(["-f", conf_arg, "-DFOREGROUND"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    while TcpStream::connect(&addr).is_err() {
        if let Ok(Some(st)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            let log = std::fs::read_to_string(root.join("logs/error.log")).unwrap_or_default();
            panic!("apache2 exited ({st}): {err}\n{log}");
        }
        assert!(Instant::now() < deadline, "apache2 did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _server = Httpd { child };
    let v = php_vm::PHP_VERSION;

    // A GET through Apache.
    let r = curl(&format!("http://127.0.0.1:{port}/e2e.php?name=world"), &[]);
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert!(r.header("Server").starts_with("Apache/2.4"), "{r:?}");
    assert!(r.header("Server").contains(&format!("PHP/{v}")), "{r:?}");
    assert_eq!(r.header("X-Powered-By"), format!("PHP/{v}"), "{r:?}");
    assert_eq!(r.header("X-E2E"), "mod_php", "{r:?}");
    assert_eq!(
        r.header("Content-Type"),
        "text/html; charset=UTF-8",
        "{r:?}"
    );
    let b = &r.body;
    assert_eq!(field(b, "sapi"), "apache2handler", "{b}");
    assert_eq!(field(b, "version"), v, "{b}");
    assert!(field(b, "software").starts_with("Apache/2.4"), "{b}");
    assert!(field(b, "apache").starts_with("Apache/2.4"), "{b}");
    assert_eq!(field(b, "mod_php"), "true", "{b}");
    assert_eq!(field(b, "script"), "/e2e.php", "{b}");
    assert_eq!(field(b, "method"), "GET", "{b}");
    assert_eq!(field(b, "name"), "world", "{b}");
    assert_eq!(field(b, "sum"), "55", "{b}");

    // A form POST.
    let r = curl(
        &format!("http://127.0.0.1:{port}/e2e.php"),
        &["--data", "a=1&b=two"],
    );
    assert_eq!(r.status, "HTTP/1.1 200 OK", "{r:?}");
    assert_eq!(field(&r.body, "method"), "POST", "{r:?}");
    assert_eq!(field(&r.body, "post"), r#"{"a":"1","b":"two"}"#, "{r:?}");
    assert_eq!(field(&r.body, "input"), "a=1&b=two", "{r:?}");

    // The directory index is the PHP script.
    std::fs::copy(root.join("docroot/e2e.php"), root.join("docroot/index.php")).unwrap();
    let r = curl(&format!("http://127.0.0.1:{port}/?name=index"), &[]);
    assert_eq!(field(&r.body, "name"), "index", "{r:?}");

    eprintln!(
        "{} with {} -> PHP {v} (apache2handler): OK",
        r.header("Server"),
        so.display()
    );
}

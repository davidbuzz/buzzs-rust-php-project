//! Full request lifecycles against an in-memory backend (no httpd): the
//! VM runs real scripts and the test inspects status, headers, body and
//! log entries the SAPI produced.

use phprust_apache2::backend::{
    Backend, DirEntry, DirStatus, IntField, LogLevel, MockBackend, StrField, Table,
};
use phprust_apache2::config::ProcessConfig;
use phprust_apache2::request::{self, RunParams, Timer};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// What a request produced.
#[derive(Debug)]
struct Response {
    status: Option<(i32, Option<Vec<u8>>, i32)>,
    content_type: Option<String>,
    headers_out: Vec<(String, String)>,
    body: String,
    logs: Vec<(LogLevel, String)>,
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("phprust-apache2-lifecycle-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Request description.
#[derive(Default, Clone)]
struct Req {
    script: &'static str,
    method: &'static str,
    query: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
    dir: Vec<(&'static str, &'static str, bool)>,
    ini: Vec<(&'static str, &'static str)>,
}

fn run(name: &str, req: Req) -> Response {
    let dir = scratch();
    let path = dir.join(name);
    std::fs::write(&path, req.script).unwrap();
    let path_s = path.to_string_lossy().into_owned();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut m = MockBackend::default()
                .with_str(StrField::Handler, b"application/x-httpd-php")
                .with_str(StrField::Filename, path_s.as_bytes())
                .with_str(
                    StrField::Uri,
                    format!(
                        "/{}",
                        Path::new(&path_s).file_name().unwrap().to_string_lossy()
                    )
                    .as_bytes(),
                )
                .with_str(
                    StrField::Method,
                    if req.method.is_empty() {
                        b"GET"
                    } else {
                        req.method.as_bytes()
                    },
                )
                .with_int(IntField::FileType, 1)
                .with_int(IntField::ProtoNum, 1001)
                .with_int(IntField::RequestTime, 1_700_000_000_250_000)
                .with_int(IntField::IsMain, 1)
                .with_entry(Table::SubprocessEnv, b"SCRIPT_FILENAME", path_s.as_bytes())
                .with_entry(
                    Table::SubprocessEnv,
                    b"REQUEST_METHOD",
                    if req.method.is_empty() {
                        b"GET"
                    } else {
                        req.method.as_bytes()
                    },
                );
            if !req.query.is_empty() {
                m = m.with_str(StrField::Args, req.query.as_bytes());
            }
            for (k, v) in &req.headers {
                m = m.with_entry(Table::HeadersIn, k.as_bytes(), v.as_bytes());
            }
            m.body_in.borrow_mut().extend_from_slice(&req.body);
            for (n, v, admin) in &req.dir {
                m.dir.borrow_mut().push(DirEntry {
                    name: (*n).to_string(),
                    value: v.as_bytes().to_vec(),
                    status: if *admin {
                        DirStatus::System
                    } else {
                        DirStatus::PerDir
                    },
                    htaccess: false,
                });
            }
            let shared = Rc::new(m);
            let mut config = ProcessConfig::with_defaults();
            for (k, v) in &req.ini {
                config.ini.push(((*k).to_string(), (*v).to_string()));
            }
            let params = RunParams {
                config: Arc::new(config),
                timer: Arc::new(Timer::default()),
                highlight: false,
            };
            request::run(Box::new(shared.clone()) as Box<dyn Backend>, &params);
            let s = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
            let resp = Response {
                status: shared.status.borrow().clone(),
                content_type: shared.content_type.borrow().as_deref().map(s),
                headers_out: shared
                    .table_entries(Table::HeadersOut)
                    .iter()
                    .map(|(k, v)| (s(k), s(v)))
                    .collect(),
                body: s(&shared.body_out.borrow()),
                logs: shared
                    .logs
                    .borrow()
                    .iter()
                    .map(|(l, m)| (*l, s(m)))
                    .collect(),
            };
            resp
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn hello_world_defaults() {
    let r = run(
        "hello.php",
        Req {
            script: "<?php echo 'Hi ', PHP_SAPI, ' ', php_sapi_name();",
            ..Req::default()
        },
    );
    assert_eq!(r.body, "Hi apache2handler apache2handler");
    assert_eq!(r.status, Some((200, None, 0)));
    assert_eq!(r.content_type.as_deref(), Some("text/html; charset=UTF-8"));
    assert_eq!(
        r.headers_out,
        vec![(
            "X-Powered-By".to_string(),
            format!("PHP/{}", php_vm::PHP_VERSION)
        )]
    );
}

#[test]
fn no_output_still_sends_headers() {
    let r = run(
        "empty.php",
        Req {
            script: "<?php http_response_code(204);",
            ..Req::default()
        },
    );
    assert_eq!(r.body, "");
    assert_eq!(r.status, Some((204, None, 0)));
}

#[test]
fn headers_and_cookies() {
    let r = run(
        "headers.php",
        Req {
            script: "<?php header('X-A: 1'); header('X-A: 2', false); header('X-B: gone'); header_remove('X-B');\n\
                     header('Content-Type: text/plain'); setcookie('n', 'v w'); setrawcookie('raw', 'x', ['path' => '/p', 'samesite' => 'Lax']);\n\
                     echo implode('|', headers_list());",
            ..Req::default()
        },
    );
    assert_eq!(
        r.body,
        format!(
            "X-Powered-By: PHP/{}|X-A: 1|X-A: 2|Content-type: text/plain;charset=UTF-8|Set-Cookie: n=v%20w|Set-Cookie: raw=x; path=/p; SameSite=Lax",
            php_vm::PHP_VERSION
        )
    );
    assert_eq!(r.content_type.as_deref(), Some("text/plain;charset=UTF-8"));
    let names: Vec<&str> = r.headers_out.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec!["X-Powered-By", "X-A", "X-A", "Set-Cookie", "Set-Cookie"]
    );
}

#[test]
fn status_line_and_redirects() {
    let r = run(
        "nf.php",
        Req {
            script: "<?php header('HTTP/1.1 404 Not Found'); echo 'x';",
            ..Req::default()
        },
    );
    assert_eq!(r.status, Some((404, Some(b"404 Not Found".to_vec()), 1001)));
    let r = run(
        "loc.php",
        Req {
            script: "<?php header('Location: /there');",
            method: "POST",
            ..Req::default()
        },
    );
    assert_eq!(r.status.map(|s| s.0), Some(303));
    assert!(r
        .headers_out
        .contains(&("Location".to_string(), "/there".to_string())));
}

#[test]
fn late_header_warns() {
    let r = run(
        "late.php",
        Req {
            script: "<?php echo 'a'; header('X: y'); var_dump(headers_sent());",
            ..Req::default()
        },
    );
    assert!(
        r.body.starts_with("a<br />\n<b>Warning</b>:  Cannot modify header information - headers already sent in <b>"),
        "{}",
        r.body
    );
    assert!(
        r.body
            .ends_with("late.php</b> on line <b>1</b><br />\nbool(true)\n"),
        "{}",
        r.body
    );
}

#[test]
fn request_variables() {
    let r = run(
        "vars.php",
        Req {
            script: "<?php echo json_encode([$_GET, $_POST, $_COOKIE, $_REQUEST, file_get_contents('php://input'), $_SERVER['PHP_SELF'], $_SERVER['REQUEST_TIME'], isset($_SERVER['argv'])]);",
            method: "POST",
            query: "g=1&a[]=x",
            headers: vec![
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Content-Length", "11"),
                ("Cookie", "c=1; d=%20"),
            ],
            body: b"p=2&a[]=yyy".to_vec(),
            ..Req::default()
        },
    );
    assert_eq!(
        r.body,
        r#"[{"g":"1","a":["x"]},{"p":"2","a":["yyy"]},{"c":"1","d":" "},{"g":"1","a":["yyy"],"p":"2","c":"1","d":" "},"p=2&a[]=yyy","\/vars.php",1700000000,false]"#
    );
}

#[test]
fn php_input_for_other_methods() {
    let r = run(
        "put.php",
        Req {
            script: "<?php var_dump(file_get_contents('php://input'), $_POST);",
            method: "PUT",
            body: b"{\"a\":1}".to_vec(),
            ..Req::default()
        },
    );
    assert_eq!(r.body, "string(7) \"{\"a\":1}\"\narray(0) {\n}\n");
}

#[test]
fn basic_auth() {
    let r = run(
        "auth.php",
        Req {
            script: "<?php echo $_SERVER['PHP_AUTH_USER'], ':', $_SERVER['PHP_AUTH_PW'];",
            headers: vec![("Authorization", "Basic dXNlcjpwYXNz")],
            ..Req::default()
        },
    );
    assert_eq!(r.body, "user:pass");
}

#[test]
fn multipart_upload() {
    let body = b"--B\r\nContent-Disposition: form-data; name=\"t\"\r\n\r\nv\r\n--B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nDATA\r\n--B--\r\n";
    let r = run(
        "up.php",
        Req {
            script: "<?php $f = $_FILES['f']; echo $_POST['t'], '|', $f['name'], '|', $f['type'], '|', $f['size'], '|', $f['error'], '|', file_get_contents($f['tmp_name']), '|', var_export(is_uploaded_file($f['tmp_name']), true), '|', strlen(file_get_contents('php://input')), '|', $f['tmp_name'];",
            method: "POST",
            headers: vec![
                ("Content-Type", "multipart/form-data; boundary=B"),
                ("Content-Length", "148"),
            ],
            body: body.to_vec(),
            ..Req::default()
        },
    );
    let parts: Vec<&str> = r.body.split('|').collect();
    assert_eq!(
        &parts[..8],
        &["v", "a.txt", "text/plain", "4", "0", "DATA", "true", "0"],
        "{}",
        r.body
    );
    assert!(
        !Path::new(parts[8]).exists(),
        "temporary upload must be removed"
    );
}

#[test]
fn post_max_size_warning() {
    let r = run(
        "big.php",
        Req {
            script: "<?php var_dump($_POST);",
            method: "POST",
            headers: vec![
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Content-Length", "20"),
            ],
            body: b"a=0123456789abcdefgh".to_vec(),
            dir: vec![("post_max_size", "10", false)],
            ..Req::default()
        },
    );
    assert_eq!(
        r.body,
        "<br />\n<b>Warning</b>:  PHP Request Startup: POST Content-Length of 20 bytes exceeds the limit of 10 bytes in <b>Unknown</b> on line <b>0</b><br />\narray(0) {\n}\n"
    );
}

#[test]
fn per_dir_and_admin_values() {
    let r = run(
        "ini.php",
        Req {
            script: "<?php var_dump(ini_get('precision'), ini_set('precision', '4'), ini_get('memory_limit'), ini_set('memory_limit', '64M'), set_time_limit(3));",
            dir: vec![("precision", "5", true), ("memory_limit", "100M", false), ("max_execution_time", "9", true)],
            ..Req::default()
        },
    );
    assert_eq!(
        r.body,
        "string(1) \"5\"\nbool(false)\nstring(4) \"100M\"\nstring(4) \"100M\"\nbool(false)\n"
    );
}

#[test]
fn fatal_without_display_is_500() {
    let r = run(
        "fatal.php",
        Req {
            script: "<?php nope();",
            dir: vec![("display_errors", "0", false)],
            ..Req::default()
        },
    );
    assert_eq!(r.body, "");
    assert_eq!(
        r.status,
        Some((500, Some(b"500 Internal Server Error".to_vec()), 1000))
    );
}

#[test]
fn error_log_goes_to_httpd() {
    let r = run(
        "log.php",
        Req {
            script: "<?php error_log('hello'); error_log('sapi', 4);",
            ..Req::default()
        },
    );
    assert_eq!(
        r.logs,
        vec![
            (LogLevel::Notice, "hello".to_string()),
            (LogLevel::Err, "sapi".to_string())
        ]
    );
}

#[test]
fn display_errors_html() {
    let r = run(
        "warn.php",
        Req {
            script: "<?php\necho $x;",
            ..Req::default()
        },
    );
    assert!(
        r.body
            .starts_with("<br />\n<b>Warning</b>:  Undefined variable $x in <b>"),
        "{}",
        r.body
    );
    assert!(
        r.body.ends_with("warn.php</b> on line <b>2</b><br />\n"),
        "{}",
        r.body
    );
}

#[test]
fn cwd_is_script_dir() {
    let r = run(
        "cwd.php",
        Req {
            script: "<?php echo getcwd();",
            ..Req::default()
        },
    );
    assert_eq!(r.body, scratch().canonicalize().unwrap().to_string_lossy());
}

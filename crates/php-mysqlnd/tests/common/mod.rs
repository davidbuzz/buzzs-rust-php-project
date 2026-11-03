//! Shared helpers for the live tests: connection parameters (MYSQL_TEST_*),
//! graceful skipping when the server is down, an oracle runner for
//! `/usr/bin/php` (mysqli) and a value renderer both sides agree on.
#![allow(dead_code)]

use std::process::Command;

use php_mysqlnd::{Connection, ResultSet};
use php_runtime::{ArrayKey, Value, ZArray};

/// Connection parameters.
pub struct Params {
    pub host: String,
    pub user: String,
    pub passwd: String,
    pub db: String,
    pub port: u32,
    pub socket: Option<String>,
}

pub fn params() -> Params {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    Params {
        host: env("MYSQL_TEST_HOST", "127.0.0.1"),
        user: env("MYSQL_TEST_USER", "phptest"),
        passwd: env("MYSQL_TEST_PASSWD", "phptest"),
        db: env("MYSQL_TEST_DB", "test"),
        port: env("MYSQL_TEST_PORT", "3306").parse().unwrap_or(3306),
        socket: std::env::var("MYSQL_TEST_SOCKET").ok(),
    }
}

/// Connects with `flags`, or returns `None` (test skipped) if the server
/// is unreachable.
pub fn connect_flags(flags: u32) -> Option<Connection> {
    let p = params();
    let c = Connection::init();
    match c.connect(
        Some(&p.host),
        Some(p.user.as_bytes()),
        Some(p.passwd.as_bytes()),
        Some(p.db.as_bytes()),
        p.port,
        p.socket.as_deref(),
        flags,
    ) {
        Ok(()) => Some(c),
        Err(e) if e.errno == 2002 || e.errno == 2006 => {
            eprintln!("skipping: MySQL server not reachable ({e})");
            None
        }
        Err(e) => panic!("connect failed: {e}"),
    }
}

pub fn connect() -> Option<Connection> {
    connect_flags(0)
}

/// Whether the oracle `/usr/bin/php` with mysqli is available.
pub fn have_php() -> bool {
    Command::new("/usr/bin/php")
        .args(["-d", "memory_limit=1G", "-r", "echo extension_loaded('mysqli') ? 'y' : 'n';"])
        .output()
        .is_ok_and(|o| o.stdout == b"y")
}

const PHP_PRELUDE: &str = r#"
mysqli_report(MYSQLI_REPORT_OFF);
function r($v){
  if (is_null($v)) return "N";
  if (is_bool($v)) return "b:".($v?"1":"0");
  if (is_int($v)) return "i:$v";
  if (is_float($v)) return "d:".bin2hex(pack("E",$v));
  if (is_string($v)) return "s:".bin2hex($v);
  if (is_array($v)) { $o=[]; foreach ($v as $k=>$x) $o[] = (is_int($k)?"i:$k":"s:".bin2hex($k))."=>".r($x); return "[".implode(",",$o)."]"; }
  return "?";
}
function conn($flags = 0) {
  $h = getenv("MYSQL_TEST_HOST") ?: "127.0.0.1"; $u = getenv("MYSQL_TEST_USER") ?: "phptest";
  $p = getenv("MYSQL_TEST_PASSWD") ?: "phptest"; $d = getenv("MYSQL_TEST_DB") ?: "test";
  $port = (int)(getenv("MYSQL_TEST_PORT") ?: 3306);
  $m = mysqli_init(); if (!$m->real_connect($h, $u, $p, $d, $port, null, $flags)) { echo "CONNECT FAILED\n"; exit(1); }
  return $m;
}
"#;

/// Runs a PHP snippet (after the prelude) with the oracle and returns stdout.
pub fn php(code: &str) -> String {
    let script = format!("{PHP_PRELUDE}\n{code}");
    let out = Command::new("/usr/bin/php")
        .args(["-d", "memory_limit=1G", "-d", "display_errors=stderr", "-r", &script])
        .output()
        .expect("run php");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Renders a value like the PHP prelude's `r()`.
pub fn r(v: &Value) -> String {
    match v {
        Value::Null => "N".into(),
        Value::Bool(b) => format!("b:{}", u8::from(*b)),
        Value::Int(i) => format!("i:{i}"),
        Value::Float(f) => format!("d:{}", hex(&f.to_le_bytes())),
        Value::Str(s) => format!("s:{}", hex(s.as_bytes())),
        Value::Array(a) => ra(a),
        _ => "?".into(),
    }
}

/// Renders an array like `r()`.
pub fn ra(a: &ZArray) -> String {
    let parts: Vec<String> = a
        .iter()
        .map(|(k, v)| {
            let k = match k {
                ArrayKey::Int(i) => format!("i:{i}"),
                ArrayKey::Str(s) => format!("s:{}", hex(s.as_bytes())),
            };
            format!("{k}=>{}", r(v))
        })
        .collect();
    format!("[{}]", parts.join(","))
}

/// Renders a row of values as a PHP list.
pub fn rrow(v: &[Value]) -> String {
    let parts: Vec<String> = v.iter().enumerate().map(|(i, x)| format!("i:{i}=>{}", r(x))).collect();
    format!("[{}]", parts.join(","))
}

/// Runs `sql` and returns the buffered result.
pub fn query(c: &Connection, sql: &str) -> Option<ResultSet> {
    c.query(sql.as_bytes()).unwrap_or_else(|e| panic!("{sql}: {e}"));
    c.store_result().unwrap()
}

/// Lowercase hex of bytes (for comparing with PHP's bin2hex).
pub fn h(b: &[u8]) -> String {
    hex(b)
}

/// A per-test table name.
pub fn table(name: &str) -> String {
    format!("mysqlnd_rs_{name}")
}

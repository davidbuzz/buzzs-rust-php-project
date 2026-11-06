//! What reaches the executor through its interrupt flag, at the SAPI level:
//!
//! * `max_execution_time`: the handler's watchdog polls the request
//!   thread's CPU clock and flags the timeout for the executor, which
//!   raises the fatal error at its next safe point — the child process
//!   stays alive.
//! * A client that went away while `ap_rwrite` wrote output: the writer has
//!   no VM, so `php_handle_aborted_connection()` runs from the module's
//!   interrupt handler before the next opcode.
//!
//! A test binary of its own, and its tests serialised: the engine's
//! interrupt and timeout flags are process-wide (one executing request per
//! httpd child, as under prefork), while the lifecycle tests run VMs
//! concurrently.

use phprust_apache2::backend::{
    Backend, DirEntry, DirStatus, IntField, LogLevel, MockBackend, StrField, Table, TableOp,
};
use phprust_apache2::config::ProcessConfig;
use phprust_apache2::handler;
use phprust_apache2::request::{RunParams, Timer};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

static SERIAL: Mutex<()> = Mutex::new(());

/// What the request produced, shared with the test thread.
#[derive(Debug, Default)]
struct Captured {
    body: String,
    status: Option<(i32, Option<Vec<u8>>, i32)>,
    logs: Vec<(LogLevel, String)>,
}

/// A [`MockBackend`] whose results are copied out when the request drops
/// it (the backend lives on the request thread).
struct Reporting {
    inner: MockBackend,
    out: Arc<Mutex<Captured>>,
}

impl Drop for Reporting {
    fn drop(&mut self) {
        let s = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
        let mut out = self.out.lock().unwrap();
        out.body = s(&self.inner.body_out.borrow());
        out.status = self.inner.status.borrow().clone();
        out.logs = self
            .inner
            .logs
            .borrow()
            .iter()
            .map(|(l, m)| (*l, s(m)))
            .collect();
    }
}

impl Backend for Reporting {
    fn str_field(&self, field: StrField) -> Option<Vec<u8>> {
        self.inner.str_field(field)
    }
    fn int_field(&self, field: IntField) -> i64 {
        self.inner.int_field(field)
    }
    fn set_user(&self, user: Option<&[u8]>) {
        self.inner.set_user(user);
    }
    fn add_cgi_vars(&self) {
        self.inner.add_cgi_vars();
    }
    fn table_entries(&self, table: Table) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.inner.table_entries(table)
    }
    fn table_get(&self, table: Table, key: &[u8]) -> Option<Vec<u8>> {
        self.inner.table_get(table, key)
    }
    fn table_op(&self, table: Table, op: TableOp, key: &[u8], value: &[u8]) {
        self.inner.table_op(table, op, key, value);
    }
    fn set_status(&self, status: i32, status_line: Option<&[u8]>, proto_num: i32) {
        self.inner.set_status(status, status_line, proto_num);
    }
    fn set_content_type(&self, content_type: &[u8]) {
        self.inner.set_content_type(content_type);
    }
    fn set_content_length(&self, len: i64) {
        self.inner.set_content_length(len);
    }
    fn write(&self, data: &[u8]) -> bool {
        self.inner.write(data)
    }
    fn flush(&self) -> bool {
        self.inner.flush()
    }
    fn body_setup(&self) -> Result<bool, i32> {
        self.inner.body_setup()
    }
    fn body_read(&self, buf: &mut [u8]) -> isize {
        self.inner.body_read(buf)
    }
    fn log(&self, level: LogLevel, msg: &[u8]) {
        self.inner.log(level, msg);
    }
    fn dir_config(&self) -> Vec<DirEntry> {
        self.inner.dir_config()
    }
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("phprust-apache2-interrupts-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs `script` through the handler's request thread and watchdog
/// (`handler::run_watched`), with `dir` as `php_value` directives.
fn run_watched(name: &str, script: &str, dir: &[(&str, &str)]) -> (Captured, PathBuf) {
    run_request(name, script, dir, false)
}

/// [`run_watched`]; with `disconnected`, the client is gone before the
/// script runs (every `ap_rwrite` / `ap_rflush` fails).
fn run_request(
    name: &str,
    script: &str,
    dir: &[(&str, &str)],
    disconnected: bool,
) -> (Captured, PathBuf) {
    let path = scratch().join(name);
    std::fs::write(&path, script).unwrap();
    let path_s = path.to_string_lossy().into_owned();
    let out = Arc::new(Mutex::new(Captured::default()));
    let dir: Vec<(String, String)> = dir
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let factory_out = out.clone();
    let make: handler::BackendFactory = Box::new(move || {
        let m = MockBackend::default()
            .with_str(StrField::Handler, b"application/x-httpd-php")
            .with_str(StrField::Filename, path_s.as_bytes())
            .with_str(StrField::Uri, b"/t.php")
            .with_str(StrField::Method, b"GET")
            .with_int(IntField::FileType, 1)
            .with_int(IntField::ProtoNum, 1001)
            .with_int(IntField::IsMain, 1)
            .with_entry(Table::SubprocessEnv, b"SCRIPT_FILENAME", path_s.as_bytes());
        *m.disconnected.borrow_mut() = disconnected;
        for (k, v) in &dir {
            m.dir.borrow_mut().push(DirEntry {
                name: k.clone(),
                value: v.as_bytes().to_vec(),
                status: DirStatus::PerDir,
                htaccess: false,
            });
        }
        Some(Box::new(Reporting {
            inner: m,
            out: factory_out,
        }) as Box<dyn Backend>)
    });
    let params = RunParams {
        config: Arc::new(ProcessConfig::with_defaults()),
        timer: Arc::new(Timer::default()),
        highlight: false,
    };
    let log = MockBackend::default();
    assert_eq!(handler::run_watched(&log, make, params), handler::OK);
    let captured = std::mem::take(&mut *out.lock().unwrap());
    (captured, path)
}

#[test]
fn busy_loop_times_out_with_a_fatal_error() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let started = Instant::now();
    let (r, path) = run_watched(
        "loop.php",
        "<?php\nregister_shutdown_function(function () { echo \"\\nstatus=\", connection_status(); });\nset_time_limit(1);\necho \"start\\n\";\nwhile (true) { $x = 1; }\n",
        &[],
    );
    assert_eq!(
        r.body,
        format!(
            "start\n<br />\n<b>Fatal error</b>:  Maximum execution time of 1 second exceeded in <b>{}</b> on line <b>5</b><br />\n\nstatus=2",
            path.display()
        )
    );
    assert_eq!(
        r.status,
        Some((200, None, 0)),
        "the headers went out with the first output"
    );
    assert!(
        started.elapsed().as_secs() < 20,
        "the executor was interrupted ({:?})",
        started.elapsed()
    );
}

#[test]
fn timeout_without_display_errors_is_a_500() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (r, _) = run_watched(
        "quiet.php",
        "<?php set_time_limit(1); while (true) { $x = 1; }\n",
        &[("display_errors", "0")],
    );
    assert_eq!(r.body, "");
    assert_eq!(
        r.status,
        Some((500, Some(b"500 Internal Server Error".to_vec()), 1000))
    );
}

#[test]
fn blocked_time_does_not_count() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // ITIMER_PROF semantics: a sleeping script burns no CPU time.
    let (r, _) = run_watched(
        "sleep.php",
        "<?php set_time_limit(1); usleep(1_300_000); echo 'done ', ini_get('max_execution_time');\n",
        &[],
    );
    assert_eq!(r.body, "done 1");
}

#[test]
fn unlimited_requests_are_not_watched() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (r, _) = run_watched(
        "unlimited.php",
        "<?php var_dump(set_time_limit(0), ini_get('max_execution_time'));\n$t = microtime(true); while (microtime(true) - $t < 1.2) { $x = 1; }\necho 'done';\n",
        &[],
    );
    assert_eq!(r.body, "bool(true)\nstring(1) \"0\"\ndone");
}

#[test]
fn lost_client_bails_out_before_the_next_statement() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let log = scratch().join("abort.log");
    let _ = std::fs::remove_file(&log);
    // php_handle_aborted_connection(): the script stops after the write
    // that failed (no "after;"), shutdown functions and destructors still
    // run and see CONNECTION_ABORTED.
    let (r, _) = run_request(
        "abort.php",
        "<?php $f = __DIR__ . '/abort.log';\nclass D { function __destruct() { file_put_contents(__DIR__ . '/abort.log', 'dtor;', FILE_APPEND); } }\n$d = new D;\nregister_shutdown_function(function () use ($f) { file_put_contents($f, 'shutdown:' . connection_status() . ':' . var_export(connection_aborted(), true) . ';', FILE_APPEND); });\necho 'a';\nfile_put_contents($f, 'after;', FILE_APPEND);\n",
        &[],
        true,
    );
    assert_eq!(r.body, "", "nothing reaches a gone client");
    assert_eq!(r.status.map(|s| s.0), Some(200));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "shutdown:1:true;dtor;"
    );
}

#[test]
fn ignored_abort_keeps_the_script_running() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let log = scratch().join("ignored.log");
    let _ = std::fs::remove_file(&log);
    let (r, _) = run_request(
        "ignored.php",
        "<?php $f = __DIR__ . '/ignored.log';\necho 'a';\nfile_put_contents($f, connection_status() . ':' . var_export(connection_aborted(), true));\n",
        &[("ignore_user_abort", "1")],
        true,
    );
    assert_eq!(r.body, "");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "1:true");
}

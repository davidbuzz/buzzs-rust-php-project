//! One PHP request: the state shared between the VM, the SAPI functions and
//! the output writer ([`RequestCtx`]), and the request lifecycle
//! ([`run`]) — `php_apache_request_ctor()`, `php_request_startup()`,
//! script execution and `php_request_shutdown()`.
//!
//! A request runs on its own thread (fresh thread-locals for the VM and the
//! extension crates, and a large stack); the context is reachable from
//! native functions through a thread-local ([`with_ctx`]).

use crate::backend::{Backend, IntField, StrField, Table, TableOp};
use crate::config::ProcessConfig;
use crate::headers::{HeaderFacts, HeaderOp, SapiHeaders};
use php_runtime::{Value, ZStr};
use php_vm::{Vm, VmOptions};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// `SAPI_POST_BLOCK_SIZE`.
pub const POST_BLOCK_SIZE: usize = 0x4000;

/// `max_execution_time` bookkeeping shared with the watchdog: the limit
/// and the request thread's CPU time when the timer was (re)started
/// (`zend_set_timeout`, which on Linux counts CPU time via
/// `ITIMER_PROF`).
#[derive(Debug, Default)]
pub struct Timer {
    limit_secs: AtomicI64,
    start_cpu_ns: AtomicI64,
}

impl Timer {
    /// Restarts the timer with `secs` (0 = unlimited); call on the request
    /// thread.
    pub fn reset(&self, secs: i64) {
        self.start_cpu_ns.store(thread_cpu_ns(), Ordering::SeqCst);
        self.limit_secs.store(secs.max(0), Ordering::SeqCst);
    }

    /// The current limit in seconds.
    pub fn limit(&self) -> i64 {
        self.limit_secs.load(Ordering::SeqCst)
    }

    /// Whether `cpu_ns` (CPU time of the request thread) exceeds the limit.
    pub fn expired(&self, cpu_ns: i64) -> bool {
        let limit = self.limit();
        limit > 0
            && cpu_ns - self.start_cpu_ns.load(Ordering::SeqCst)
                >= limit.saturating_mul(1_000_000_000)
    }
}

/// CPU time of the calling thread in nanoseconds.
pub fn thread_cpu_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes into the timespec we own.
    unsafe {
        libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts);
    }
    ts.tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec)
}

/// The request body as seen by `php://input`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Not read yet: `php://input` reads it from the server on demand.
    Unread,
    /// Read at startup (`sapi_read_standard_form_data`).
    Buffered(Vec<u8>),
    /// Consumed by the multipart parser (`php://input` is empty).
    Consumed,
}

/// State of one request shared by the VM thread's components.
pub struct RequestCtx {
    /// The web server side.
    pub backend: Box<dyn Backend>,
    /// `SG(sapi_headers)`.
    pub headers: RefCell<SapiHeaders>,
    /// Method, protocol and default content type.
    pub facts: RefCell<HeaderFacts>,
    /// `PG(connection_status) & PHP_CONNECTION_ABORTED`.
    pub aborted: Cell<bool>,
    /// The request body.
    pub body: RefCell<Body>,
    /// Whether `ap_setup_client_block` ran (and whether a body follows).
    body_ready: Cell<Option<bool>>,
    /// Bytes of the body read so far (`SG(read_post_bytes)`).
    pub read_post_bytes: Cell<i64>,
    /// `max_execution_time` timer.
    pub timer: Arc<Timer>,
    /// `header_register_callback()` callable.
    pub header_callback: RefCell<Option<Value>>,
}

impl std::fmt::Debug for RequestCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestCtx")
            .field("headers", &self.headers)
            .field("aborted", &self.aborted)
            .finish_non_exhaustive()
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<RequestCtx>>> = const { RefCell::new(None) };
}

/// Runs `f` with the current request context (`None` outside a request).
pub fn with_ctx<R>(f: impl FnOnce(&RequestCtx) -> R) -> Option<R> {
    let ctx = CURRENT.with(|c| c.borrow().clone())?;
    Some(f(&ctx))
}

impl RequestCtx {
    /// A context for `backend`.
    pub fn new(backend: Box<dyn Backend>, timer: Arc<Timer>) -> Self {
        let status = i32::try_from(backend.int_field(IntField::Status)).unwrap_or(200);
        let facts = HeaderFacts {
            method: backend.str_field(StrField::Method).unwrap_or_default(),
            proto_num: i32::try_from(backend.int_field(IntField::ProtoNum)).unwrap_or(1001),
            ..HeaderFacts::default()
        };
        RequestCtx {
            backend,
            headers: RefCell::new(SapiHeaders::new(status)),
            facts: RefCell::new(facts),
            aborted: Cell::new(false),
            body: RefCell::new(Body::Unread),
            body_ready: Cell::new(None),
            read_post_bytes: Cell::new(0),
            timer,
            header_callback: RefCell::new(None),
        }
    }

    /// Applies a header operation.
    pub fn header_op(&self, op: HeaderOp<'_>) -> Result<bool, crate::headers::HeaderError> {
        let facts = self.facts.borrow();
        self.headers.borrow_mut().op(op, &facts, &*self.backend)
    }

    /// `sapi_send_headers()` (no-op once sent).
    pub fn send_headers(&self) {
        let facts = self.facts.borrow();
        self.headers.borrow_mut().send(&facts, &*self.backend);
    }

    /// Whether headers were sent.
    pub fn headers_sent(&self) -> bool {
        self.headers.borrow().sent
    }

    /// `php_apache_sapi_ub_write()`.
    pub fn write(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.send_headers();
        if !self.aborted.get() && !self.backend.write(data) {
            self.aborted.set(true);
        }
    }

    /// `php_apache_sapi_flush()` (without sending headers: see
    /// [`ApacheOutput`]).
    pub fn flush(&self) {
        if self.headers_sent() && !self.aborted.get() && !self.backend.flush() {
            self.aborted.set(true);
        }
    }

    /// Prepares reading the request body; `false` when there is none.
    fn body_available(&self) -> bool {
        if let Some(b) = self.body_ready.get() {
            return b;
        }
        let ok = self.backend.body_setup().unwrap_or(false);
        self.body_ready.set(Some(ok));
        ok
    }

    /// `sapi_module.read_post()`: fills `buf` as far as possible; returns
    /// the number of bytes read (0 at the end).
    pub fn read_post(&self, buf: &mut [u8]) -> usize {
        if !self.body_available() {
            return 0;
        }
        let mut total = 0;
        while total < buf.len() {
            let n = self.backend.body_read(&mut buf[total..]);
            let Ok(n) = usize::try_from(n) else {
                break;
            };
            if n == 0 {
                break;
            }
            total += n;
        }
        self.read_post_bytes
            .set(self.read_post_bytes.get() + i64::try_from(total).unwrap_or(0));
        total
    }

    /// Reads the rest of the body.
    pub fn read_rest(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; POST_BLOCK_SIZE];
        loop {
            let n = self.read_post(&mut buf);
            out.extend_from_slice(&buf[..n]);
            if n < buf.len() {
                break;
            }
        }
        out
    }

    /// The body for `php://input`.
    pub fn input(&self) -> Vec<u8> {
        let current = self.body.borrow().clone();
        match current {
            Body::Buffered(b) => b,
            Body::Consumed => Vec::new(),
            Body::Unread => {
                let b = self.read_rest();
                *self.body.borrow_mut() = Body::Buffered(b.clone());
                b
            }
        }
    }

    /// Re-reads the INI values cached by the SAPI (default content type,
    /// timer limit is handled by the callers).
    pub fn refresh_facts(&self, vm: &Vm) {
        let mut f = self.facts.borrow_mut();
        f.default_mimetype = vm.ini_string("default_mimetype").unwrap_or_default();
        f.default_charset = vm.ini_string("default_charset").unwrap_or_default();
    }
}

/// The VM's standard output: `ub_write` to Apache, sending the headers
/// before the first byte.  `flush()` is a no-op: the VM flushes its stdout
/// before log writes and at shutdown, which must neither commit the
/// headers nor force a chunked response (Apache computes Content-Length
/// for responses that end without an intermediate flush).  PHP's
/// `flush()` is implemented by the SAPI functions instead.
pub struct ApacheOutput(pub Rc<RequestCtx>);

impl std::fmt::Debug for ApacheOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApacheOutput")
    }
}

impl Write for ApacheOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `E_ERROR | E_PARSE | E_COMPILE_ERROR | E_USER_ERROR | E_RECOVERABLE_ERROR`:
/// the levels after which `php_error_cb` turns a 200 into a 500 when
/// errors are not displayed.
pub fn is_fatal_level(level: i64) -> bool {
    matches!(level, 1 | 4 | 64 | 256 | 4096)
}

/// `PG(display_errors)` as a boolean.
pub fn display_errors(vm: &Vm) -> bool {
    let v = vm.ini_string("display_errors").unwrap_or_default();
    let v = v.to_ascii_lowercase();
    match v.as_slice() {
        b"on" | b"yes" | b"true" | b"stdout" | b"stderr" => true,
        _ => crate::headers::strtol(&v) != 0,
    }
}

/// The 500 status of `php_error_cb` for a fatal error with
/// `display_errors` off, applied when the request's last error is fatal.
pub fn apply_fatal_status(vm: &Vm) {
    let fatal = vm
        .last_error
        .as_ref()
        .is_some_and(|e| is_fatal_level(e.level));
    if !fatal || display_errors(vm) {
        return;
    }
    with_ctx(|ctx| {
        if !ctx.headers_sent() && ctx.headers.borrow().response_code == 200 {
            let _ = ctx.header_op(HeaderOp::Replace(b"HTTP/1.0 500 Internal Server Error", 0));
        }
    });
}

/// Applies the merged `php_value`-family directives (`apply_config()`):
/// `php_admin_*` with `PHP_INI_SYSTEM` access (which also locks the entry
/// against `ini_set`), the others with `PHP_INI_PERDIR`, `.htaccess` ones
/// at the HTACCESS stage.
pub fn apply_dir_config(vm: &mut Vm, entries: &[crate::backend::DirEntry]) {
    use php_support::ini::{Access, Stage};
    let mut sink: Vec<php_support::Diagnostic> = Vec::new();
    for e in entries {
        let access = match e.status {
            crate::backend::DirStatus::PerDir => Access::PERDIR,
            crate::backend::DirStatus::System => Access::SYSTEM,
        };
        let stage = if e.htaccess {
            Stage::Htaccess
        } else {
            Stage::Activate
        };
        vm.ini
            .alter(&e.name, &e.value, access, stage, false, &mut sink);
    }
    sync_cached_ini(vm);
}

/// Mirrors the VM's cached copies of `error_reporting`, `precision` and
/// `serialize_precision` after INI changes made outside `ini_set()`.
pub fn sync_cached_ini(vm: &mut Vm) {
    vm.error_reporting = match vm.ini.get("error_reporting") {
        Some(v) if !v.is_empty() => std::str::from_utf8(v)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0),
        _ => 30719,
    };
    vm.precision = vm.ini.get_long("precision");
    vm.serialize_precision = vm.ini.get_long("serialize_precision");
}

/// Parameters of [`run`].
#[derive(Debug, Clone)]
pub struct RunParams {
    /// The process configuration.
    pub config: Arc<ProcessConfig>,
    /// The timer watched by the handler thread.
    pub timer: Arc<Timer>,
    /// `application/x-httpd-php-source`: highlight instead of executing.
    pub highlight: bool,
}

/// `(user, password, digest)` of HTTP authentication.
pub type AuthData = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

/// `php_handle_auth_data()`: `(user, password, digest)` from an
/// `Authorization` header.
pub fn auth_data(auth: Option<&[u8]>) -> AuthData {
    let Some(auth) = auth.filter(|a| !a.is_empty()) else {
        return (None, None, None);
    };
    if auth.len() >= 6 && auth[..6].eq_ignore_ascii_case(b"Basic ") {
        let decoded = base64_decode(&auth[6..]);
        let decoded = decoded.split(|&c| c == 0).next().unwrap_or_default();
        if let Some(colon) = decoded.iter().position(|&c| c == b':') {
            return (
                Some(decoded[..colon].to_vec()),
                Some(decoded[colon + 1..].to_vec()),
                None,
            );
        }
    }
    if auth.len() >= 7 && auth[..7].eq_ignore_ascii_case(b"Digest ") {
        return (None, None, Some(auth[7..].to_vec()));
    }
    (None, None, None)
}

/// Non-strict `php_base64_decode()`: skips invalid characters, stops at
/// padding.
pub fn base64_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
        }
    }
    out
}

/// Escapes `s` for a single-quoted PHP string literal.
fn php_single_quoted(s: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &c in s {
        if c == b'\'' || c == b'\\' {
            out.push(b'\\');
        }
        out.push(c);
    }
    out.push(b'\'');
    out
}

/// Runs one request on the current thread (`php_handler()` from
/// `php_apache_request_ctor()` to `php_apache_request_dtor()`).
pub fn run(backend: Box<dyn Backend>, params: &RunParams) {
    let ctx = Rc::new(RequestCtx::new(backend, params.timer.clone()));
    CURRENT.with(|c| *c.borrow_mut() = Some(ctx.clone()));

    let opts = VmOptions {
        ini: params.config.ini.clone(),
    };
    let exts = crate::extensions::all();
    let refs: Vec<&dyn php_vm::ext::Extension> = exts.iter().map(AsRef::as_ref).collect();
    let mut vm = Vm::with_extensions(&opts, Box::new(ApacheOutput(ctx.clone())), &refs);
    apply_dir_config(&mut vm, &ctx.backend.dir_config());
    ctx.refresh_facts(&vm);
    params.timer.reset(vm.ini_long("max_execution_time"));

    // php_apache_request_ctor()
    let headers_in_auth = ctx.backend.table_get(Table::HeadersIn, b"Authorization");
    let (mut user, password, digest) = auth_data(headers_in_auth.as_deref());
    if user.is_none() {
        user = ctx.backend.str_field(StrField::User);
    }
    ctx.backend.set_user(user.as_deref());
    for h in ["Content-Length", "Last-Modified", "Expires", "ETag"] {
        ctx.backend
            .table_op(Table::HeadersOut, TableOp::Unset, h.as_bytes(), b"");
    }
    if vm.ini_bool("last_modified") {
        ctx.backend.set_last_modified();
    }

    // php_request_startup()
    vm.script_path = ZStr::from("Unknown");
    if vm.ini_bool("expose_php") {
        let line = format!("X-Powered-By: PHP/{}", php_vm::PHP_VERSION);
        let _ = ctx.header_op(HeaderOp::Replace(line.as_bytes(), 0));
    }
    crate::vars::populate(
        &mut vm,
        &ctx,
        &crate::vars::Auth {
            user,
            password,
            digest,
        },
    );
    install_input_hook(&ctx);
    vm.shutdown_functions
        .insert(0, (Value::from(crate::functions::FATAL_CHECK), Vec::new()));

    let filename = ctx
        .backend
        .str_field(StrField::Filename)
        .unwrap_or_default();
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&filename));
    let saved_cwd = std::env::current_dir().ok();
    if let Some(dir) = path.parent() {
        let _ = std::env::set_current_dir(dir);
    }
    let script = String::from_utf8_lossy(&filename).into_owned();
    if params.highlight {
        let mut src = b"<?php highlight_file(".to_vec();
        src.extend_from_slice(&php_single_quoted(&filename));
        src.extend_from_slice(b");");
        vm.run_script(&src, &script);
    } else {
        let src = std::fs::read(&path).unwrap_or_default();
        vm.run_script(&src, &script);
    }
    finish(&mut vm, &ctx);
    if let Some(cwd) = saved_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
    drop(vm);
    CURRENT.with(|c| *c.borrow_mut() = None);
}

/// After the script: `php_output_deactivate()` sends the headers if no
/// output was produced; uploaded files that were not moved are deleted.
fn finish(vm: &mut Vm, ctx: &RequestCtx) {
    apply_fatal_status(vm);
    call_header_callback(vm, ctx);
    ctx.send_headers();
    crate::multipart::cleanup(&std::mem::take(&mut vm.rfc1867_uploaded_files));
}

/// Invokes the `header_register_callback()` callable once, before the
/// headers are sent from a point where the VM is available.
pub fn call_header_callback(vm: &mut Vm, ctx: &RequestCtx) {
    if ctx.headers_sent() {
        return;
    }
    let cb = ctx.header_callback.borrow_mut().take();
    if let Some(cb) = cb {
        let _ = vm.call_value(&cb, Vec::new());
    }
}

/// Makes `php://input` read the request body through the SAPI (the
/// streams layer's output/filename hooks are re-installed as
/// `php-ext-streams` sets them up).
fn install_input_hook(ctx: &Rc<RequestCtx>) {
    use php_ext_streams::bridge::with_vm;
    let body_ctx = ctx.clone();
    php_streams::state::set_hooks(php_streams::state::Hooks {
        output: Some(Box::new(|data: &[u8]| {
            let data = data.to_vec();
            let _ = with_vm(|vm| vm.output(&data));
        })),
        executed_filename: Some(Box::new(|| {
            with_vm(|vm| vm.current_location().0.as_bytes().to_vec())
        })),
        request_body: Some(Box::new(move || body_ctx.input())),
    });
    php_streams::state::update_settings(|s| s.sapi_is_cli = false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockBackend;

    #[test]
    fn auth_parsing() {
        assert_eq!(
            auth_data(Some(b"Basic dXNlcjpwYXNzOndvcmQ=")),
            (Some(b"user".to_vec()), Some(b"pass:word".to_vec()), None)
        );
        assert_eq!(
            auth_data(Some(b"basic dXNlcg==")),
            (None, None, None),
            "no colon: not basic auth"
        );
        assert_eq!(
            auth_data(Some(b"Digest username=\"x\"")),
            (None, None, Some(b"username=\"x\"".to_vec()))
        );
        assert_eq!(auth_data(None), (None, None, None));
    }

    #[test]
    fn base64_is_lenient() {
        assert_eq!(base64_decode(b"aGVs bG8="), b"hello");
        assert_eq!(base64_decode(b"aGVsbG8"), b"hello");
    }

    #[test]
    fn timer_expiry() {
        let t = Timer::default();
        t.reset(0);
        assert!(!t.expired(i64::MAX));
        t.reset(1);
        let start = t.start_cpu_ns.load(Ordering::SeqCst);
        assert!(!t.expired(start + 999_999_999));
        assert!(t.expired(start + 1_000_000_000));
    }

    #[test]
    fn ctx_output_sends_headers_once() {
        let ctx = RequestCtx::new(Box::new(MockBackend::default()), Arc::new(Timer::default()));
        ctx.flush();
        assert!(
            !ctx.headers_sent(),
            "flush before output must not send headers"
        );
        ctx.write(b"hi");
        assert!(ctx.headers_sent());
        ctx.write(b" there");
        ctx.flush();
        assert!(!ctx.aborted.get());
    }

    #[test]
    fn ctx_reads_body() {
        let m = MockBackend::default();
        m.body_in.borrow_mut().extend_from_slice(b"abc");
        let ctx = RequestCtx::new(Box::new(m), Arc::new(Timer::default()));
        assert_eq!(ctx.input(), b"abc");
        assert_eq!(ctx.input(), b"abc");
        assert_eq!(ctx.read_post_bytes.get(), 3);
    }

    #[test]
    fn quoting() {
        assert_eq!(php_single_quoted(b"a'b\\c"), b"'a\\'b\\\\c'");
    }
}

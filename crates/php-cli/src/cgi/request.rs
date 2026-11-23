// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The request path `php-cgi` and `php-fpm` share: `fpm_main.c` is a fork of
//! `cgi_main.c` over the same `main/fastcgi.c`, with the same SAPI layer
//! (`sapi_cgibin_ub_write`, `sapi_cgi_send_headers`, `sapi_cgi_read_post`,
//! `sapi_cgi_register_variables`, `init_request_info`). The configuration
//! helpers, the VM each request gets, and one `FastCGI` request.

use crate::config::Config;
use crate::{env, extensions, fastcgi};
use php_cli_server::parser::Method;
use php_cli_server::request::Request;
use php_cli_server::sapi::{
    CgiInfo, CgiReply, CliServerModule, FastCgiStreams, PreparedVm, RequestCtx, ServerOutput,
};
use php_runtime::{Value, ZStr};
use php_vm::ext::{Args, Extension, VmResult};
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stack the VM needs for deep recursion in user code.
pub(crate) const VM_STACK: u64 = 256 << 20;

/// The INI defaults of the CGI SAPI where they differ from what the VM
/// applies for the CLI (`main.c` defaults; the CGI SAPI has no
/// `ini_defaults` and no `HARDCODED_INI`).
pub(crate) const SAPI_INI: &[(&str, &str)] = &[
    ("display_errors", "1"),
    ("html_errors", "1"),
    ("implicit_flush", "0"),
    ("max_execution_time", "30"),
    ("register_argc_argv", "0"),
];

/// An INI boolean as `OnUpdateBool` reads it (`zend_ini_parse_bool`).
pub(crate) fn ini_bool(v: &str) -> bool {
    let v = v.trim();
    v.eq_ignore_ascii_case("on")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("true")
        || v.trim_start_matches(['+', '-'])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<i64>()
            .is_ok_and(|n| n != 0)
}

/// A `cgi.*` boolean from the configuration, else its default.
pub(crate) fn cfg_bool(cfg: &Config, name: &str, default: bool) -> bool {
    cfg.get(name).map_or(default, ini_bool)
}

/// The `cgi.*` settings `init_request_info()` reads.
pub(crate) fn request_settings(cfg: &Config) -> env::Settings {
    env::Settings {
        fix_pathinfo: cfg_bool(cfg, "cgi.fix_pathinfo", true),
        discard_path: cfg_bool(cfg, "cgi.discard_path", false),
        doc_root: cfg
            .get("doc_root")
            .filter(|v| !v.is_empty())
            .map(|v| v.as_bytes().to_vec()),
        script_path_encoded: cfg_bool(cfg, "fastcgi.script_path_encoded", true),
    }
}

/// Which SAPI module the VMs register (`cgi-fcgi` or `fpm-fcgi`).
static FPM: AtomicBool = AtomicBool::new(false);

/// Makes this binary's VMs the FPM SAPI's (`php-fpm`).
pub(crate) fn set_fpm() {
    FPM.store(true, Ordering::Relaxed);
}

fn sapi_module() -> CliServerModule {
    CliServerModule {
        cgi: true,
        fpm: FPM.load(Ordering::Relaxed),
    }
}

/// A response decided from the request's information and variables.
pub(crate) type ReplyHook<'a> = &'a dyn Fn(&env::RequestInfo, &env::Env) -> Option<CgiReply>;
/// Called with the request's information and variables.
pub(crate) type RequestHook<'a> = &'a dyn Fn(&env::RequestInfo, &env::Env);

/// See [`RequestHooks::done`].
pub(crate) type DoneHook<'a> = &'a dyn Fn(&RequestDone);

/// `init_request_info()`.
pub(crate) type RequestInfoFn = fn(&mut env::Env, &env::Settings, &dyn env::Fs) -> env::RequestInfo;

/// What a SAPI adds to a `FastCGI` request.
#[derive(Default)]
pub(crate) struct RequestHooks<'a> {
    /// INI entries after the configuration's (FPM: the pool's
    /// `php_value`s, the request's `PHP_VALUE`).
    pub extra_ini: Vec<(String, String)>,
    /// A response instead of the script, decided once the request's
    /// variables are known (FPM's status and ping pages, its "File not
    /// found." and `security.limit_extensions`).
    pub reply: Option<ReplyHook<'a>>,
    /// Called once the script starts (FPM's `fpm_request_executing()`).
    pub executing: Option<RequestHook<'a>>,
    /// Called on the request's VM before it runs (FPM's slowlog handler).
    pub vm_setup: Option<fn(&mut Vm)>,
    /// FPM's error logger: "Unable to open primary script", and
    /// `fastcgi.error_header` applies.
    pub open_failure_log: Option<fn(&str)>,
    /// Called with the outcome before the `FastCGI` request is ended
    /// (FPM's scoreboard and access log).
    pub done: Option<DoneHook<'a>>,
    /// The SAPI's `init_request_info()` in place of php-cgi's (FPM's).
    pub request_info: Option<RequestInfoFn>,
    /// Entries no script may change (`ini_set()` fails): FPM's
    /// `php_admin_value`s and `PHP_ADMIN_VALUE`s.
    pub locked: Vec<String>,
    /// The SAPI's `log_message` for `log_errors` messages, in place of
    /// php-cgi's (each message as its own `FCGI_STDERR` line when
    /// `fastcgi.logging` is on): FPM logs them itself.
    pub log: Option<fn(&[u8])>,
}

/// What a `FastCGI` request leaves for the SAPI's bookkeeping (FPM's
/// access log and scoreboard).
#[derive(Debug)]
pub(crate) struct RequestDone {
    /// `EG(exit_status)`.
    pub exit_status: i32,
    /// `SG(sapi_headers).http_response_code`.
    pub response_code: i32,
    /// The request's environment after `init_request_info()`.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    /// The request's information.
    pub info: env::RequestInfo,
    /// The response's header lines (`SG(sapi_headers).headers`).
    pub headers: Vec<Vec<u8>>,
}

/// One `FastCGI` request: `init_request_info()` on its environment, the
/// body from its `STDIN` stream, the script with output and log messages
/// as records, then `fcgi_finish_request()` (before the VM is torn down,
/// as `sapi_cgi_deactivate()` ends the request in request shutdown).
pub(crate) fn fastcgi_request(
    cfg: &Config,
    settings: &env::Settings,
    logging: bool,
    params: fastcgi::Env,
    conn: &Rc<fastcgi::Connection>,
    hooks: &RequestHooks<'_>,
) -> RequestDone {
    let mut environment = env::Env::new(params);
    let info = hooks.request_info.unwrap_or(env::init_request_info)(
        &mut environment,
        settings,
        &env::RealFs,
    );
    let reply = hooks.reply.and_then(|f| f(&info, &environment));
    if reply.is_none() {
        if let Some(f) = hooks.executing {
            f(&info, &environment);
        }
    }
    let content = usize::try_from(info.content_length)
        .ok()
        .filter(|&n| n > 0)
        .map(|n| conn.read_body(n));
    // `apache_request_headers()` walks the request's variables as
    // `fcgi_loadenv()` does: newest name first (the hash prepends).
    let newest_first: Vec<(Vec<u8>, Vec<u8>)> = environment.vars().iter().rev().cloned().collect();
    let request = build_request(&info, content, &newest_first);
    let php_self = env::php_self(
        info.request_uri.as_deref(),
        environment.get("PATH_INFO").as_deref(),
        settings.fix_pathinfo,
    );
    let cgi_info = CgiInfo {
        php_self,
        fcgi_env: Some(environment.vars().to_vec()),
        reply,
        fpm_log: hooks.open_failure_log,
        ..CgiInfo::default()
    };
    let streams: Rc<dyn FastCgiStreams> = conn.clone();
    let ctx = Rc::new(RequestCtx::new_cgi(request, cgi_info).with_fastcgi(streams));
    if info.response_code != 200 {
        let _ = ctx.header_op(php_cli_server::headers::HeaderOp::SetStatus(
            info.response_code,
        ));
    }
    let mut ini = sapi_ini(cfg);
    ini.extend(hooks.extra_ini.iter().cloned());
    let server_cfg = php_cli_server::sapi::ServerConfig {
        document_root: Vec::new(),
        host: String::new(),
        port: 0,
        router: None,
        ini: ini.clone(),
        extensions: extensions::all,
    };
    let mut prepared = worker_vm(cfg, &ini, hooks.vm_setup);
    let slot = prepared.slot.clone();
    for name in &hooks.locked {
        // `fpm_php_zend_ini_alter_master()` with `ZEND_INI_SYSTEM`: the
        // entry's `modifiable` becomes `PHP_INI_SYSTEM`.
        if let Some(v) = prepared.vm.ini.get(name).map(<[u8]>::to_vec) {
            let mut sink = Vec::new();
            prepared.vm.ini.alter(
                name,
                &v,
                php_support::ini::Access::SYSTEM,
                php_support::ini::Stage::Activate,
                true,
                &mut sink,
            );
        }
    }
    if let Some(pt) = &info.path_translated {
        activate_config(
            &mut prepared.vm,
            cfg,
            pt,
            environment.get("SERVER_NAME").as_deref(),
            environment.get("DOCUMENT_ROOT").as_deref(),
        );
    }
    if let Some(f) = hooks.log {
        prepared.vm.log_hook = Some(Box::new(move |msg: &[u8]| f(msg)));
    } else if logging {
        // `sapi_cgi_log_message()`: `log_errors` messages as FCGI_STDERR.
        let c = conn.clone();
        prepared.vm.log_hook = Some(Box::new(move |msg: &[u8]| {
            let mut line = msg.to_vec();
            line.push(b'\n');
            let _ = c.stderr(&line);
        }));
    }
    let (_, mut vm) = php_cli_server::sapi::run_request(prepared, &ctx, &server_cfg, false);
    let done = RequestDone {
        exit_status: ctx.exit_status.get(),
        response_code: ctx.headers.borrow().response_code,
        env: environment.vars().to_vec(),
        info,
        headers: ctx.headers.borrow().headers.clone(),
    };
    // FPM's `fpm_request_end()` and `fpm_log_write()` come before
    // `php_request_shutdown()` ends the request: a client that has its
    // response finds the access log written.
    if let Some(f) = hooks.done {
        f(&done);
    }
    // `zend_deactivate()` restores the INI entries before
    // `sapi_deactivate()` ends the request: what the restore reports still
    // reaches the web server.
    vm.ini_deactivate();
    conn.finish_request(false);
    drop(ctx);
    keep_worker_vm(ini, PreparedVm { vm, slot });
    done
}

/// A worker's VM and the INI settings it was made from.
type KeptVm = (Vec<(String, String)>, PreparedVm);

thread_local! {
    /// The FastCGI worker's VM between requests (see [`worker_vm`]).
    static WORKER_VM: RefCell<Option<KeptVm>> = const { RefCell::new(None) };
}

/// The VM for a `FastCGI` request: the worker's from its earlier requests
/// when it was made from the same INI settings (module startup runs once
/// per process, as in php-src), else a new one, set up by `setup`.
fn worker_vm(cfg: &Config, ini: &[(String, String)], setup: Option<fn(&mut Vm)>) -> PreparedVm {
    let kept = WORKER_VM
        .with(|w| w.borrow_mut().take())
        .filter(|(made_from, _)| made_from.as_slice() == ini)
        .map(|(_, p)| p);
    if let Some(p) = kept {
        return p;
    }
    let mut p = new_prepared_vm(cfg, ini.to_vec());
    if let Some(f) = setup {
        f(&mut p.vm);
    }
    p
}

/// `php_request_shutdown()` for the worker: its VM goes back to its
/// start-of-request state for the next request (`Vm::request_reset`, as
/// `mod_php` and `php -S` do), without the connection's log hook.
fn keep_worker_vm(ini: Vec<(String, String)>, mut prepared: PreparedVm) {
    prepared.vm.log_hook = None;
    prepared.vm.request_reset();
    WORKER_VM.with(|w| *w.borrow_mut() = Some((ini, prepared)));
}

/// A directory's `.user.ini` entries and when they expire
/// (`user_config_cache_entry`).
type UserIni = (i64, Vec<(String, Vec<u8>)>);

thread_local! {
    /// `CGIG(user_config_cache)`: by directory, kept across requests.
    static USER_INI_CACHE: RefCell<std::collections::HashMap<Vec<u8>, UserIni>> =
        RefCell::new(std::collections::HashMap::new());
}

/// The entries of an INI file (sections flattened, `[PATH=...]` and
/// `[HOST=...]` sections and arrays left out, as `php_ini_activate_config()`
/// only uses plain entries).
struct UserIniEntries {
    entries: Vec<(String, Vec<u8>)>,
    special: bool,
}

impl php_support::ini::IniParserCallback for UserIniEntries {
    fn section(&mut self, name: &[u8]) {
        let lower = name.to_ascii_lowercase();
        self.special = lower.starts_with(b"path") || lower.starts_with(b"host");
    }

    fn entry(&mut self, key: &[u8], value: Option<php_support::ini::IniScalar>) {
        if self.special {
            return;
        }
        let key = String::from_utf8_lossy(key).into_owned();
        let v = value.map(|v| v.to_bytes()).unwrap_or_default();
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push((key, v));
    }

    fn pop_entry(&mut self, _key: &[u8], _value: php_support::ini::IniScalar, _offset: &[u8]) {}
}

/// `php_parse_user_ini_file()`: `dir/filename` when it is a regular file.
fn parse_user_ini_file(dir: &[u8], filename: &str, into: &mut UserIniEntries) {
    use std::os::unix::ffi::OsStrExt;
    let mut path = dir.to_vec();
    path.push(b'/');
    path.extend_from_slice(filename.as_bytes());
    let p = std::path::Path::new(std::ffi::OsStr::from_bytes(&path));
    if !p.is_file() {
        return;
    }
    let Ok(text) = std::fs::read(p) else {
        return;
    };
    let shown = String::from_utf8_lossy(&path).into_owned();
    into.special = false;
    let mut hooks = php_support::ini::FnHooks {
        constant: crate::config::startup_constant,
        variable: |name: &[u8]| {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            std::env::var_os(std::ffi::OsStr::from_bytes(name)).map(std::ffi::OsString::into_vec)
        },
    };
    let _ = php_support::ini::parse_ini(
        &text,
        &shown,
        php_support::ini::ScannerMode::Normal,
        &mut hooks,
        into,
    );
}

/// `sapi_cgi_pre_request_init()` / `php_cgi_ini_activate_user_config()`:
/// the `user_ini.filename` files from the document root down to the
/// script's directory (only the script's own directory when it is outside
/// the document root), cached for `user_ini.cache_ttl` seconds, applied in
/// `PHP_INI_PERDIR` mode (so `php_admin_value`s and system settings stay).
/// `sapi_cgi_activate()`: the php.ini `[HOST=]` section of the lowercased
/// `SERVER_NAME` (`php_ini_activate_per_host_config()`), the `[PATH=]`
/// sections from `/` down to the script's directory
/// (`php_ini_activate_per_dir_config()`), both in `PHP_INI_SYSTEM` mode at
/// the activate stage, then the `user_ini.filename` files.
pub(crate) fn activate_config(
    vm: &mut Vm,
    cfg: &Config,
    path_translated: &[u8],
    server_name: Option<&[u8]>,
    doc_root: Option<&[u8]>,
) {
    if let Some(name) = server_name {
        let host = name.to_ascii_lowercase();
        activate_system_entries(vm, cfg.configuration.host_entries(&host));
    }
    let dir = script_dir(path_translated);
    activate_system_entries(vm, cfg.configuration.path_entries(&dir));
    activate_user_ini(vm, path_translated, doc_root);
}

/// `php_ini_activate_config(..., PHP_INI_SYSTEM, PHP_INI_STAGE_ACTIVATE)`:
/// `zend_alter_ini_entry_ex()` for each entry, through its handlers.
fn activate_system_entries(vm: &mut Vm, entries: Vec<(String, Vec<u8>)>) {
    for (k, v) in entries {
        if k == "memory_limit" || k == "max_memory_limit" {
            // `zend_set_memory_limit()` compares with the heap in use.
            vm.ini.memory_usage = u64::try_from(php_memlimit::real_usage()).unwrap_or(u64::MAX);
        }
        let mut sink = Vec::new();
        if vm.ini.alter(
            &k,
            &v,
            php_support::ini::Access::SYSTEM,
            php_support::ini::Stage::Activate,
            false,
            &mut sink,
        ) {
            let _ = vm.run_ini_modify_hook(&k, &v, php_vm::ext::IniStage::Activate);
            vm.sync_ini(&k);
        }
    }
}

/// The script's directory with a slash appended, as `sapi_cgi_activate()`
/// builds it: `zend_dirname()`, then `DEFAULT_SLASH` (`//` for a script
/// in `/`).
fn script_dir(path_translated: &[u8]) -> Vec<u8> {
    let mut path = match path_translated.iter().rposition(|&c| c == b'/') {
        Some(0) => b"/".to_vec(),
        Some(i) => path_translated[..i].to_vec(),
        None => b".".to_vec(),
    };
    path.push(b'/');
    path
}

fn activate_user_ini(vm: &mut Vm, path_translated: &[u8], doc_root: Option<&[u8]>) {
    let filename = String::from_utf8_lossy(&vm.ini_string("user_ini.filename").unwrap_or_default())
        .into_owned();
    if filename.is_empty() {
        return;
    }
    let Some(doc_root) = doc_root else {
        return;
    };
    let mut doc_root = doc_root.to_vec();
    if doc_root.last() == Some(&b'/') {
        doc_root.pop();
    }
    let path = script_dir(path_translated);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let ttl = vm.ini_long("user_ini.cache_ttl");
    let cached = USER_INI_CACHE.with(|c| {
        c.borrow()
            .get(&path)
            .filter(|(expires, _)| now <= *expires)
            .map(|(_, e)| e.clone())
    });
    let entries = cached.unwrap_or_else(|| {
        let mut found = UserIniEntries {
            entries: Vec::new(),
            special: false,
        };
        let mut scan = path.clone();
        if !scan.starts_with(b"/") {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            if let Ok(r) = std::fs::canonicalize(std::ffi::OsStr::from_bytes(&scan)) {
                scan = r.into_os_string().into_vec();
            }
        }
        let (s1, s2) = if scan.len() > doc_root.len() {
            (&doc_root, &scan)
        } else {
            (&scan, &doc_root)
        };
        let n = s1.len().min(s2.len());
        if s1[..n] == s2[..n] {
            // Each directory from the document root to the script's.
            let mut at = doc_root.len().min(s2.len());
            while let Some(off) = s2[at..].iter().position(|&c| c == b'/') {
                let end = at + off;
                parse_user_ini_file(&scan[..end.min(scan.len())], &filename, &mut found);
                at = end + 1;
            }
        } else {
            parse_user_ini_file(&scan, &filename, &mut found);
        }
        USER_INI_CACHE.with(|c| {
            c.borrow_mut()
                .insert(path.clone(), (now + ttl, found.entries.clone()));
        });
        found.entries
    });
    for (k, v) in entries {
        let mut sink = Vec::new();
        let accepted = vm.ini.alter(
            &k,
            &v,
            php_support::ini::Access::PERDIR,
            php_support::ini::Stage::Htaccess,
            false,
            &mut sink,
        );
        if accepted {
            // `zend_alter_ini_entry_ex()` runs the entry's on-modify
            // handler (extensions keeping their own copy see the value).
            let _ = vm.run_ini_modify_hook(&k, &v, php_vm::ext::IniStage::Htaccess);
            vm.sync_ini(&k);
        }
    }
}

/// The INI entries for the VM: the SAPI defaults, then the configuration.
pub(crate) fn sapi_ini(cfg: &Config) -> Vec<(String, String)> {
    SAPI_INI
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .chain(cfg.entries.iter().cloned())
        .collect()
}

/// The request as the built-in server's machinery sees it; `vars` is the
/// request's environment (the process environment of a CGI request).
pub(crate) fn build_request(
    info: &env::RequestInfo,
    content: Option<Vec<u8>>,
    vars: &[(Vec<u8>, Vec<u8>)],
) -> Request {
    let method_name = info.request_method.clone().unwrap_or_default();
    let mut request = Request {
        method: method_from(&method_name),
        protocol_version: 100,
        query_string: info.query_string.clone(),
        path_translated: info.path_translated.clone(),
        content,
        ..Request::default()
    };
    if !info.content_type.is_empty() {
        request
            .headers
            .push((b"content-type".to_vec(), info.content_type.clone()));
    }
    // `read_cookies`: `HTTP_COOKIE` of the request's environment.
    if let Some((_, cookie)) = vars.iter().find(|(k, _)| k == b"HTTP_COOKIE") {
        request.headers.push((b"cookie".to_vec(), cookie.clone()));
    }
    if let Some(auth) = &info.authorization {
        request
            .headers
            .push((b"authorization".to_vec(), auth.clone()));
    }
    for (k, v) in vars {
        if let Some(name) = env::request_header_name(k) {
            request.headers_original_case.push((name, v.clone()));
        }
    }
    request
}

/// `SG(request_info).request_method` as the server's method enum.
pub(crate) fn method_from(name: &[u8]) -> Method {
    const METHODS: &[Method] = &[
        Method::Delete,
        Method::Get,
        Method::Head,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Query,
        Method::Connect,
        Method::Options,
        Method::Trace,
        Method::Copy,
        Method::Lock,
        Method::Mkcol,
        Method::Move,
        Method::Mkcalendar,
        Method::Propfind,
        Method::Proppatch,
        Method::Search,
        Method::Unlock,
        Method::Report,
        Method::Mkactivity,
        Method::Checkout,
        Method::Merge,
        Method::MSearch,
        Method::Notify,
        Method::Subscribe,
        Method::Unsubscribe,
    ];
    METHODS
        .iter()
        .copied()
        .find(|m| m.as_str().as_bytes() == name)
        .unwrap_or(if name.is_empty() {
            Method::Get
        } else {
            Method::NotImplemented
        })
}

/// `php_ini_opened_path` and `php_ini_scanned_files`.
type IniFiles = (Option<Vec<u8>>, Option<Vec<u8>>);

thread_local! {
    static INI_FILES: RefCell<IniFiles> = const { RefCell::new((None, None)) };
}

#[allow(clippy::unnecessary_wraps)]
pub(crate) fn php_ini_loaded_file(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(INI_FILES.with(|f| {
        f.borrow()
            .0
            .as_deref()
            .map_or(Value::Bool(false), |s| Value::Str(ZStr::from(s)))
    }))
}

#[allow(clippy::unnecessary_wraps)]
pub(crate) fn php_ini_scanned_files(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(INI_FILES.with(|f| {
        f.borrow()
            .1
            .as_deref()
            .map_or(Value::Bool(false), |s| Value::Str(ZStr::from(s)))
    }))
}

/// Creates the request's VM (module startup) writing to the request
/// context's output.
pub(crate) fn new_prepared_vm(cfg: &Config, ini: Vec<(String, String)>) -> PreparedVm {
    let opts = VmOptions { ini };
    let mut exts = extensions::all();
    exts.push(Box::new(sapi_module()));
    let refs: Vec<&dyn Extension> = exts.iter().map(AsRef::as_ref).collect();
    let slot = Rc::new(RefCell::new(None));
    let mut vm = Vm::with_extensions(&opts, Box::new(ServerOutput(slot.clone())), &refs);
    configure(&mut vm, cfg);
    PreparedVm { vm, slot }
}

/// What every VM of this binary gets after module startup.
pub(crate) fn configure(vm: &mut Vm, cfg: &Config) {
    vm.set_configuration(cfg.configuration.values.clone());
    vm.set_native_stack_size(usize::try_from(VM_STACK).unwrap_or(usize::MAX));
    INI_FILES.with(|f| *f.borrow_mut() = (cfg.opened_path.clone(), cfg.scanned_files.clone()));
    vm.register_function("php_ini_loaded_file(): string|false", php_ini_loaded_file);
    vm.register_function(
        "php_ini_scanned_files(): string|false",
        php_ini_scanned_files,
    );
}

/// A VM writing straight to standard output (lint mode and the tests of
/// the shared configuration code).
pub(crate) fn new_vm(cfg: &Config) -> Vm {
    let opts = VmOptions { ini: sapi_ini(cfg) };
    let stdout: Box<dyn Write> = Box::new(std::io::stdout());
    let mut exts = extensions::all();
    exts.push(Box::new(sapi_module()));
    let refs: Vec<&dyn Extension> = exts.iter().map(AsRef::as_ref).collect();
    let mut vm = Vm::with_extensions(&opts, stdout, &refs);
    configure(&mut vm, cfg);
    vm
}

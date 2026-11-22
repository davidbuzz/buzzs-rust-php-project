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
    let info = env::init_request_info(&mut environment, settings, &env::RealFs);
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
    let request = build_request(&info, content, environment.vars());
    let php_self = env::php_self(
        info.request_uri.as_deref(),
        environment.get("PATH_INFO").as_deref(),
        settings.fix_pathinfo,
    );
    let cgi_info = CgiInfo {
        php_self,
        fcgi_env: Some(environment.vars().to_vec()),
        reply,
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
    let mut prepared = new_prepared_vm(cfg, ini);
    if logging {
        // `sapi_cgi_log_message()`: `log_errors` messages as FCGI_STDERR.
        let c = conn.clone();
        prepared.vm.log_hook = Some(Box::new(move |msg: &[u8]| {
            let mut line = msg.to_vec();
            line.push(b'\n');
            let _ = c.stderr(&line);
        }));
    }
    let (_, vm) = php_cli_server::sapi::run_request(prepared, &ctx, &server_cfg, false);
    conn.finish_request(false);
    let done = RequestDone {
        exit_status: ctx.exit_status.get(),
        response_code: ctx.headers.borrow().response_code,
        env: environment.vars().to_vec(),
        info,
    };
    drop(ctx);
    drop(vm);
    done
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

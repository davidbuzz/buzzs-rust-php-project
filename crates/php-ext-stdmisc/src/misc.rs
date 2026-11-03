//! `mail()`, `error_log()`, `assert_options()`, `phpinfo()` and
//! `phpcredits()`.

use crate::credits;
use crate::ctx::{global, RtExt};
use php_runtime::{Value, ZArray, ZStr};
use php_stdmisc::info::{self, InfoModule, InfoPrinter, InfoSource};
use php_stdmisc::mail::{self, ErrorLogSettings, HeadersArg, LogTarget, MailSettings, ScriptInfo};
use php_stdmisc::misc::{self, AssertState};
use php_stdmisc::system::{self, PageInfo};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `mail.*` INI settings.
fn mail_settings(vm: &Vm) -> MailSettings {
    let s = |n: &str| vm.ini_string(n).unwrap_or_default();
    MailSettings {
        sendmail_path: s("sendmail_path"),
        force_extra_parameters: s("mail.force_extra_parameters"),
        add_x_header: vm.ini_bool("mail.add_x_header"),
        log: s("mail.log"),
        cr_lf_mode: vm
            .ini_string("mail.cr_lf_mode")
            .unwrap_or_else(|| b"crlf".to_vec()),
        mixed_lf_and_crlf: vm.ini_bool("mail.mixed_lf_and_crlf"),
    }
}

/// The executing script's file, line and owner.
fn script_info(vm: &Vm) -> ScriptInfo {
    let (file, line) = vm.current_location();
    let path = vm.script_path.as_bytes();
    let page = PageInfo::for_script((!path.is_empty()).then_some(path));
    ScriptInfo {
        filename: file.as_bytes().to_vec(),
        lineno: line,
        uid: page.uid,
    }
}

/// `php_format_date("d-M-Y H:i:s e", ts)` through the date extension
/// (UTC formatting when `date()` is unavailable).
fn log_date(vm: &mut Vm, ts: i64) -> String {
    if vm.native_function("date").is_none() {
        return mail::format_log_date_utc(ts);
    }
    match vm.call_value(
        &Value::from("date"),
        vec![Value::from("d-M-Y H:i:s e"), Value::Int(ts)],
    ) {
        Ok(Value::Str(s)) => s.to_string_lossy(),
        _ => mail::format_log_date_utc(ts),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn mail_fn(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (to, subject, message) = (a.str(0), a.str(1), a.str(2));
    let headers = a.at(3);
    let params = a.has(4).then(|| a.str(4));
    let harg = match &headers {
        Value::Str(s) => HeadersArg::Str(s.as_bytes()),
        Value::Array(arr) => HeadersArg::Array(arr),
        _ => HeadersArg::None,
    };
    let settings = mail_settings(vm);
    let script = script_info(vm);
    let date = log_date(vm, now());
    mail::mail(
        to.as_bytes(),
        subject.as_bytes(),
        message.as_bytes(),
        harg,
        params.as_ref().map(ZStr::as_bytes),
        &settings,
        &script,
        &|_| date.clone(),
        vm,
    )
    .vm(vm)
    .map(Value::Bool)
}

/// The CLI logger with dates from the date extension.
struct VmLog {
    date: String,
}

impl LogTarget for VmLog {
    fn sapi_log_message(&mut self, message: &[u8]) -> bool {
        mail::CliLogTarget.sapi_log_message(message)
    }

    fn format_date(&mut self, _ts: i64) -> String {
        self.date.clone()
    }
}

fn error_log(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let message = a.str(0);
    let ty = if a.has(1) { a.int(1) } else { 0 };
    let dest = a.opt(2).map(|_| a.str(2));
    let headers = a.opt(3).map(|_| a.str(3));
    let settings = ErrorLogSettings {
        error_log: vm.ini_string("error_log").filter(|v| !v.is_empty()),
        error_log_mode: vm
            .ini_string("error_log_mode")
            .and_then(|v| i64::from_str_radix(String::from_utf8_lossy(&v).trim(), 8).ok())
            .unwrap_or(0o644),
    };
    if ty == 3 && dest.as_ref().is_none_or(|d| d.is_empty()) {
        // `php_stream_open_wrapper()` on an empty path.
        return Err(vm.throw("ValueError", "Path must not be empty"));
    }
    let mail_settings = mail_settings(vm);
    let script = script_info(vm);
    let mut target = VmLog {
        date: log_date(vm, now()),
    };
    vm.flush_stdout();
    mail::error_log(
        message.as_bytes(),
        ty,
        dest.as_ref().map(ZStr::as_bytes),
        headers.as_ref().map(ZStr::as_bytes),
        &settings,
        &mail_settings,
        &script,
        &mut target,
        vm,
    )
    .vm(vm)
    .map(Value::Bool)
}

thread_local! {
    /// The callback set with `assert_options(ASSERT_CALLBACK, ...)`.
    static ASSERT_CALLBACK: std::cell::RefCell<Option<Value>> = const { std::cell::RefCell::new(None) };
}

fn assert_options(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    vm.raise(8192, "Function assert_options() is deprecated since 8.3")?;
    let mut st = AssertState::default();
    for name in [
        "assert.active",
        "assert.bail",
        "assert.warning",
        "assert.exception",
        "assert.callback",
    ] {
        let v = vm.ini_string(name).unwrap_or_default();
        st.on_ini_update(name, &v);
    }
    st.callback = ASSERT_CALLBACK.with(|c| c.borrow().clone());
    let value = a.get(1);
    let (old, change) = st.assert_options(a.int(0), value.as_ref(), vm).vm(vm)?;
    ASSERT_CALLBACK.with(|c| c.borrow_mut().clone_from(&st.callback));
    if let Some(ch) = change {
        let mut sink: Vec<php_support::Diagnostic> = Vec::new();
        if vm
            .ini
            .ini_set(ch.name, ch.value.as_bytes(), &mut sink)
            .is_none()
        {
            return Ok(Value::Bool(false));
        }
    }
    Ok(old)
}

/// A loaded extension as a `phpinfo()` section.
struct Module {
    name: String,
    ini: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

impl InfoModule for Module {
    fn name(&self) -> &str {
        &self.name
    }
    fn has_section(&self) -> bool {
        true
    }
    fn print_body(&self, p: &mut InfoPrinter) {
        p.ini_entries(&self.ini);
    }
}

/// `display_ini_entries()` rows for an extension.
fn ini_rows(vm: &Vm, module: &str) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let Ok(mut entries) = vm.ini.ini_get_all(Some(module)) else {
        return Vec::new();
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
        .into_iter()
        .map(|e| {
            (
                e.name.into_bytes(),
                info::ini_display(e.local_value.as_deref(), true),
                info::ini_display(e.global_value.as_deref(), true),
            )
        })
        .collect()
}

/// Names returned by a PHP function returning a list of strings
/// (`stream_get_wrappers()` & co.), or `default`.
fn string_list(vm: &mut Vm, func: &str, default: &[&str]) -> Vec<Vec<u8>> {
    if vm.native_function(func).is_some() {
        if let Ok(Value::Array(a)) = vm.call_value(&Value::from(func), Vec::new()) {
            return a
                .values()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(s.as_bytes().to_vec()),
                    _ => None,
                })
                .collect();
        }
    }
    default.iter().map(|s| s.as_bytes().to_vec()).collect()
}

fn constant_str(vm: &Vm, name: &str, default: &str) -> String {
    match vm.constants.get(&ZStr::from(name)) {
        Some(Value::Str(s)) => s.to_string_lossy(),
        _ => default.to_string(),
    }
}

/// The general rows of `phpinfo()` from "PHP API" to "`DTrace` Support".
const ENGINE_ROWS: &[(&str, &str)] = &[
    ("PHP API", "20240925"),
    ("PHP Extension", "20240925"),
    ("Zend Extension", "420240925"),
    ("Zend Extension Build", "API420240925,NTS"),
    ("PHP Extension Build", "API20240925,NTS"),
    ("PHP Integer Size", "64 bits"),
    ("Debug Build", "no"),
    ("Thread Safety", "disabled"),
    ("Zend Signal Handling", "enabled"),
    ("Zend Memory Manager", "enabled"),
    ("Zend Multibyte Support", "disabled"),
    ("Zend Max Execution Timers", "disabled"),
    ("IPv6 Support", "enabled"),
    ("DTrace Support", "disabled"),
];

/// The `$_SERVER` `PHP_SELF`/`PHP_AUTH_*` strings and the request arrays
/// shown in the "PHP Variables" section.
#[allow(clippy::type_complexity)]
fn request_vars(vm: &Vm) -> (Vec<(&'static str, Vec<u8>)>, Vec<(&'static str, ZArray)>) {
    let server = match global(vm, b"_SERVER") {
        Some(Value::Array(s)) => s,
        _ => ZArray::new(),
    };
    let mut php_vars = Vec::new();
    for k in ["PHP_SELF", "PHP_AUTH_USER", "PHP_AUTH_PW", "PHP_AUTH_TYPE"] {
        if let Some(Value::Str(s)) = server.get_str(k.as_bytes()).map(Value::deref) {
            php_vars.push((k, s.as_bytes().to_vec()));
        }
    }
    let mut gpcse = Vec::new();
    for k in [
        "_REQUEST", "_GET", "_POST", "_COOKIE", "_FILES", "_SERVER", "_ENV",
    ] {
        if let Some(Value::Array(arr)) = global(vm, k.as_bytes()) {
            if !arr.is_empty() || k == "_SERVER" {
                gpcse.push((k, arr));
            }
        }
    }
    (php_vars, gpcse)
}

fn phpinfo(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let flag = if a.has(0) { a.int(0) } else { info::INFO_ALL };
    let version = constant_str(vm, "PHP_VERSION", "8.5.0-dev");
    let uname = system::php_uname(b"a").vm(vm)?;
    let wrappers = string_list(
        vm,
        "stream_get_wrappers",
        &[
            "https",
            "ftps",
            "compress.zlib",
            "php",
            "file",
            "glob",
            "data",
            "http",
            "ftp",
            "phar",
        ],
    );
    let transports = string_list(
        vm,
        "stream_get_transports",
        &[
            "tcp", "udp", "unix", "udg", "ssl", "tls", "tlsv1.0", "tlsv1.1", "tlsv1.2", "tlsv1.3",
        ],
    );
    let filters = string_list(
        vm,
        "stream_get_filters",
        &[
            "zlib.*",
            "string.rot13",
            "string.toupper",
            "string.tolower",
            "convert.*",
            "consumed",
            "dechunk",
        ],
    );
    let core_ini = ini_rows(vm, "Core");
    let mut names: Vec<String> = php_vm::LOADED_EXTENSIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for e in &vm.loaded_extensions {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(e)) {
            names.push(e.clone());
        }
    }
    let modules: Vec<Module> = names
        .into_iter()
        .map(|name| {
            let ini = ini_rows(vm, &name);
            Module { name, ini }
        })
        .collect();
    let module_refs: Vec<&dyn InfoModule> = modules.iter().map(|m| m as &dyn InfoModule).collect();
    let (php_vars, gpcse) = request_vars(vm);
    let zend_version = format!(
        "Zend Engine v{}, Copyright (c) Zend Technologies\n",
        constant_str(vm, "ZEND_VERSION", "4.5.0-dev")
    );
    let src = InfoSource {
        php_version: &version,
        uname: uname.as_bytes(),
        build_date: "Jan  1 2026 00:00:00",
        build_rows: &[("Configure Command", " './configure' ")],
        server_api: Some("Command Line Interface"),
        config_file_path: "/usr/local/lib",
        loaded_ini: None,
        scan_dir: None,
        scanned_files: None,
        engine_rows: ENGINE_ROWS,
        stream_wrappers: &wrappers,
        stream_transports: &transports,
        stream_filters: &filters,
        zend_version: &zend_version,
        core_ini: &core_ini,
        modules: &module_refs,
        php_vars: &php_vars,
        gpcse: &gpcse,
    };
    // `php-stdmisc` does not render the credits section: print everything
    // up to the variables, then the credits, then the license.
    let mut out = info::print_info(flag & !(info::INFO_CREDITS | info::INFO_LICENSE), &src, true, vm)
        .vm(vm)?;
    if flag & info::INFO_CREDITS != 0 {
        let mut p = InfoPrinter::new(true);
        p.hr();
        out.extend_from_slice(&p.out);
        out.extend_from_slice(&credits_text(CREDITS_ALL & !CREDITS_FULLPAGE));
    }
    if flag & info::INFO_LICENSE != 0 {
        let lic = info::print_info(info::INFO_LICENSE, &src, true, vm).vm(vm)?;
        out.extend_from_slice(lic.strip_prefix(b"phpinfo()\n".as_slice()).unwrap_or(&lic));
    }
    vm.output(&out);
    Ok(Value::Bool(true))
}

/// `PHP_CREDITS_ALL`.
const CREDITS_ALL: i64 = 0xFFFF_FFFF;
/// `PHP_CREDITS_FULLPAGE`.
const CREDITS_FULLPAGE: i64 = 32;

/// `php_print_credits()` in text mode.
fn credits_text(flag: i64) -> Vec<u8> {
    let mut p = InfoPrinter::new(true);
    p.print("PHP Credits\n");
    if flag & 1 != 0 {
        p.table_start();
        p.colspan_header(1, "PHP Group");
        p.table_row(&[b"Thies C. Arntzen, Stig Bakken, Shane Caraveo, Andi Gutmans, Rasmus Lerdorf, Sam Ruby, Sascha Schumann, Zeev Suraski, Jim Winstead, Andrei Zmievski"]);
        p.table_end();
    }
    if flag & 2 != 0 {
        p.table_start();
        p.colspan_header(1, "Language Design & Concept");
        p.table_row(&[b"Andi Gutmans, Rasmus Lerdorf, Zeev Suraski, Marcus Boerger"]);
        p.table_end();
        p.table_start();
        p.colspan_header(2, "PHP Authors");
        p.table_header(&[b"Contribution", b"Authors"]);
        for (k, v) in credits::GENERAL {
            p.table_row(&[k.as_bytes(), v.as_bytes()]);
        }
        p.table_end();
    }
    for (bit, title, header, rows) in [
        (4, "SAPI Modules", "Contribution", credits::SAPI),
        (8, "Module Authors", "Module", credits::EXT),
    ] {
        if flag & bit != 0 {
            p.table_start();
            p.colspan_header(2, title);
            p.table_header(&[header.as_bytes(), b"Authors"]);
            for (k, v) in rows {
                p.table_row(&[k.as_bytes(), v.as_bytes()]);
            }
            p.table_end();
        }
    }
    if flag & 16 != 0 {
        p.table_start();
        p.colspan_header(2, "PHP Documentation");
        for (k, v) in credits::DOCS {
            p.table_row(&[k.as_bytes(), v.as_bytes()]);
        }
        p.table_end();
    }
    if flag & 64 != 0 {
        p.table_start();
        p.colspan_header(1, "PHP Quality Assurance Team");
        p.table_row(&[credits::QA.as_bytes()]);
        p.table_end();
    }
    if flag & 128 != 0 {
        p.table_start();
        p.colspan_header(2, "Websites and Infrastructure team");
        for (k, v) in credits::WEB {
            p.table_row(&[k.as_bytes(), v.as_bytes()]);
        }
        p.table_end();
    }
    p.out
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn phpcredits(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let flag = if a.has(0) { a.int(0) } else { CREDITS_ALL };
    vm.output(&credits_text(flag));
    Ok(Value::Bool(true))
}

/// Registers the functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for (name, v) in info::INFO_CONSTANTS.iter().chain(misc::ASSERT_CONSTANTS) {
        m.constant(name, Value::Int(*v));
    }
    m.function(
        "mail(string $to, string $subject, string $message, array|string $additional_headers = [], string $additional_params = \"\"): bool",
        mail_fn,
    )
    .function(
        "error_log(string $message, int $message_type = 0, ?string $destination = null, ?string $additional_headers = null): bool",
        error_log,
    )
    .function("assert_options(int $option, mixed $value = UNKNOWN): mixed", assert_options)
    .function("phpinfo(int $flags = INFO_ALL): true", phpinfo)
    .function("phpcredits(int $flags = CREDITS_ALL): true", phpcredits);
}

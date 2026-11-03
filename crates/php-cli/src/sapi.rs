//! The CLI SAPI's own symbols (`php_cli.c`'s `additional_functions`,
//! `php_cli_process_title.c`), the configuration-file functions backed
//! by the SAPI's `php_init_config()` result, and the CLI request
//! variables (`sapi_cli_register_variables`).

use crate::config::Config;
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;
use std::cell::RefCell;
use std::os::unix::ffi::OsStrExt;

/// `php_ini_opened_path` and `php_ini_scanned_files`.
type IniFiles = (Option<Vec<u8>>, Option<Vec<u8>>);

thread_local! {
    /// The `header_register_callback()` callable of the request.
    static HEADER_CALLBACK: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INI_FILES: RefCell<IniFiles> = const { RefCell::new((None, None)) };
}

fn str_value(s: &[u8]) -> Value {
    Value::Str(ZStr::from(s))
}

#[allow(clippy::unnecessary_wraps)]
fn php_ini_loaded_file(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(INI_FILES.with(|f| {
        f.borrow()
            .0
            .as_deref()
            .map_or(Value::Bool(false), str_value)
    }))
}

#[allow(clippy::unnecessary_wraps)]
fn php_ini_scanned_files(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(INI_FILES.with(|f| {
        f.borrow()
            .1
            .as_deref()
            .map_or(Value::Bool(false), str_value)
    }))
}

fn cli_set_process_title(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let title = a.str(0);
    if crate::proctitle::set(title.as_bytes()) {
        return Ok(Value::Bool(true));
    }
    vm.php_warning("cli_set_process_title had an error: Not initialized correctly")?;
    Ok(Value::Bool(false))
}

fn cli_get_process_title(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    if let Some(t) = crate::proctitle::get() {
        return Ok(str_value(&t));
    }
    vm.php_warning("cli_get_process_title had an error: Not initialized correctly")?;
    Ok(Value::Null)
}

/// `header_register_callback()`: the callable runs when the headers are
/// sent — never once output has started (the CLI sends its empty header
/// set with the first output), otherwise at the end of the request.
#[allow(clippy::unnecessary_wraps)]
fn header_register_callback(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cb = a.values.first().cloned().unwrap_or(Value::Null);
    HEADER_CALLBACK.with(|c| *c.borrow_mut() = Some(cb));
    Ok(Value::Bool(true))
}

/// Request shutdown: `sapi_send_headers()` if nothing was output.
fn send_headers(vm: &mut Vm) {
    let cb = HEADER_CALLBACK.with(|c| c.borrow_mut().take());
    if let Some(cb) = cb {
        if !vm.headers_sent {
            let _ = vm.call_value(&cb, Vec::new());
        }
    }
}

/// Registers the CLI functions and constants on a freshly created VM.
pub fn register(vm: &mut Vm, cfg: &Config) {
    INI_FILES.with(|f| *f.borrow_mut() = (cfg.opened_path.clone(), cfg.scanned_files.clone()));
    vm.register_function("php_ini_loaded_file(): string|false", php_ini_loaded_file);
    vm.register_function(
        "php_ini_scanned_files(): string|false",
        php_ini_scanned_files,
    );
    vm.register_function(
        "cli_set_process_title(string $title): bool",
        cli_set_process_title,
    );
    vm.register_function("cli_get_process_title(): ?string", cli_get_process_title);
    if vm.native_function("header_register_callback").is_none() {
        vm.register_function(
            "header_register_callback(callable $callback): true",
            header_register_callback,
        );
        HEADER_CALLBACK.with(|c| *c.borrow_mut() = None);
        vm.shutdown_hooks.insert(0, send_headers);
    }
    vm.constants.insert(
        ZStr::from("PHP_CLI_PROCESS_TITLE"),
        Value::Bool(crate::proctitle::available()),
    );
    vm.constants
        .entry(ZStr::from("PHP_BUILD_DATE"))
        .or_insert_with(|| Value::Str(ZStr::from(crate::BUILD_DATE)));
}

/// What `sapi_cli_register_variables` needs to know about the request.
pub struct Request<'a> {
    /// `php_self` (the script as given, or `Standard input code`).
    pub php_self: &'a [u8],
    /// `script_filename` (empty for stdin and `-r`).
    pub script_filename: &'a [u8],
    /// `SG(request_info).argv`.
    pub argv: &'a [Vec<u8>],
}

fn env_table() -> HashTable {
    let mut t = HashTable::new();
    for (k, v) in std::env::vars_os() {
        t.insert(ArrayKey::from_bytes(k.as_bytes()), str_value(v.as_bytes()));
    }
    t
}

/// Sets `$argv`, `$argc`, `$_SERVER`, `$_ENV` and the empty request
/// arrays the way the CLI does (`variables_order` decides whether
/// `$_SERVER`/`$_ENV` are filled).
pub fn register_variables(vm: &mut Vm, req: &Request<'_>) {
    let order = vm.ini_string("variables_order").unwrap_or_default();
    let has = |c: u8| order.iter().any(|x| x.eq_ignore_ascii_case(&c));
    let values: Vec<Value> = req.argv.iter().map(|a| str_value(a)).collect();
    let count = Value::Int(i64::try_from(values.len()).unwrap_or(0));
    let argv = Value::Array(ZArray::from_values(values));

    let mut server = HashTable::new();
    if has(b'S') {
        server = env_table();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        for (k, v) in [
            ("PHP_SELF", req.php_self),
            ("SCRIPT_NAME", req.php_self),
            ("SCRIPT_FILENAME", req.script_filename),
            ("PATH_TRANSLATED", req.script_filename),
            ("DOCUMENT_ROOT", b"".as_slice()),
        ] {
            server.insert(ArrayKey::from(k), str_value(v));
        }
        server.insert(
            ArrayKey::from("REQUEST_TIME_FLOAT"),
            Value::Float(now.as_secs_f64()),
        );
        server.insert(
            ArrayKey::from("REQUEST_TIME"),
            Value::Int(i64::try_from(now.as_secs()).unwrap_or(0)),
        );
        server.insert(ArrayKey::from("argv"), argv.clone());
        server.insert(ArrayKey::from("argc"), count.clone());
    }
    let env = if has(b'E') {
        env_table()
    } else {
        HashTable::new()
    };
    let mut g = vm.globals.borrow_mut();
    let mut set = |name: &str, v: Value| {
        let i = g.find_or_add(name.as_bytes()) as usize;
        g.slots[i] = Some(v);
    };
    for name in ["_GET", "_POST", "_COOKIE", "_FILES", "_REQUEST"] {
        set(name, Value::Array(ZArray::new()));
    }
    set("_ENV", Value::Array(ZArray::from_table(env)));
    set("_SERVER", Value::Array(ZArray::from_table(server)));
    set("argv", argv);
    set("argc", count);
}

/// Sets a global variable (`$argn`, `$argi`).
pub fn set_global(vm: &mut Vm, name: &str, v: Value) {
    let mut g = vm.globals.borrow_mut();
    let i = g.find_or_add(name.as_bytes()) as usize;
    g.slots[i] = Some(v);
}

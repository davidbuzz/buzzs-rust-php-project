//! The phprust engine: compiler and virtual machine.
//!
//! * [`compiler`] — compiles a `php_parser` AST into op arrays
//!   (`Zend/zend_compile.c`).
//! * [`Vm`] — the interpreter (`Zend/zend_execute.c`): call frames, classes,
//!   objects, exceptions, generators, closures, includes, error reporting
//!   and the builtin function library.
//!
//! The CLI (`crates/php-cli`) creates a [`Vm`] with [`Vm::new`], sets the
//! command line arguments and runs a script with [`Vm::run_script`].

pub use php_parser::ast;

mod attrs;
mod builtins;
mod call;
pub mod class;
mod classlink;
mod closure;
pub mod compiler;
mod context;
mod enums;
mod errors;
mod exceptions;
mod exec;
pub mod ext;
mod fiber;
pub mod func;
mod generator;
pub mod gc;
mod include;
pub mod interrupt;
mod pfa;
mod iter;
mod lazy;
pub mod object;
pub mod opcode;
mod place;
mod props;
mod reflection;
mod typecheck;
pub mod types;
mod variance;
pub mod vm;
mod weakref;

pub use vm::{Throw, Vm, VmResult};

use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_support::ini::{Access, IniDirective, IniRegistry, ModifyHandler};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::rc::Rc;

/// The PHP version implemented (php-src master).
pub const PHP_VERSION: &str = "8.7.0-dev";
/// The Zend engine version.
pub const ZEND_VERSION: &str = "4.7.0-dev";
/// Extensions reported by `get_loaded_extensions()`.
pub const LOADED_EXTENSIONS: &[&str] = &[
    "Core",
    "date",
    "pcre",
    "random",
    "Reflection",
    "SPL",
    "standard",
    "hash",
];

/// Options for creating a VM.
#[derive(Debug, Default, Clone)]
pub struct VmOptions {
    /// `-d name=value` settings (applied after the CLI overrides).
    pub ini: Vec<(String, String)>,
}

fn extra_directives() -> Vec<IniDirective> {
    let d = |n: &str, v: Option<&str>, h: ModifyHandler| {
        IniDirective::new(n, v, Access::ALL, h, "Core")
    };
    vec![
        d(
            "include_path",
            Some(".:/usr/share/php"),
            ModifyHandler::None,
        ),
        d(
            "zend.exception_string_param_max_len",
            Some("15"),
            ModifyHandler::ExceptionStringParamMaxLen,
        ),
        d("assert.active", Some("1"), ModifyHandler::Bool),
        d("assert.bail", Some("0"), ModifyHandler::Bool),
        d("assert.warning", Some("1"), ModifyHandler::Bool),
        d("assert.callback", None, ModifyHandler::None),
        d("assert.exception", Some("1"), ModifyHandler::Bool),
        d("date.timezone", Some("UTC"), ModifyHandler::None),
        d("arg_separator", Some("&"), ModifyHandler::None),
        d("pcre.backtrack_limit", Some("1000000"), ModifyHandler::Long),
        d("pcre.recursion_limit", Some("100000"), ModifyHandler::Long),
        d("pcre.jit", Some("1"), ModifyHandler::Bool),
        d("highlight.bg", Some("#FFFFFF"), ModifyHandler::None),
        d("mbstring.language", Some("neutral"), ModifyHandler::None),
        d("zend.enable_gc", Some("1"), ModifyHandler::Bool),
    ]
}

impl Vm {
    /// Creates a VM with PHP's CLI defaults and the given ini settings.
    pub fn new(opts: &VmOptions, stdout: Box<dyn Write>) -> Vm {
        let mut config: Vec<(String, Vec<u8>)> = php_support::ini::CLI_OVERRIDES
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.as_bytes().to_vec()))
            .collect();
        for (k, v) in &opts.ini {
            config.push((k.clone(), v.as_bytes().to_vec()));
        }
        let mut sink: Vec<php_support::Diagnostic> = Vec::new();
        let mut ini = IniRegistry::with_core(config.clone(), &mut sink);
        for d in extra_directives() {
            ini.register(d, &mut sink);
        }
        let empty_code = Rc::new(opcode::OpArray::default());
        let mut vm = Vm {
            frames: Vec::new(),
            functions: HashMap::new(),
            classes: HashMap::new(),
            class_order: Vec::new(),
            constants: HashMap::new(),
            user_constants: Vec::new(),
            globals: Rc::new(RefCell::new(vm::VarStore::default())),
            ini,
            output: php_support::output::OutputStack::new(),
            stdout,
            error_reporting: 30719,
            error_handlers: Vec::new(),
            exception_handlers: Vec::new(),
            included: Vec::new(),
            included_set: HashSet::new(),
            autoloaders: Vec::new(),
            autoloading: HashSet::new(),
            exit_status: 0,
            shutdown_functions: Vec::new(),
            last_error: None,
            pending_throw: None,
            script_path: ZStr::empty(),
            argv: Vec::new(),
            bound_decls: HashMap::new(),
            units: Vec::new(),
            precision: 14,
            serialize_precision: -1,
            in_shutdown: false,
            in_error_handler: 0,
            ext: builtins::ExtState::default(),
            anon_counter: 0,
            depth: 0,
            once_flags: HashSet::new(),
            tick_functions: Vec::new(),
            interrupt_handlers: Vec::new(),
            tick_hooks: Vec::new(),
            user_ticks: Vec::new(),
            ticks_count: 0,
            fiber_switch_block: 0,
            timeout_seconds: 0,
            headers_sent: false,
            fibers: Vec::new(),
            loaded_extensions: Vec::new(),
            shutdown_hooks: Vec::new(),
            startup_hooks: Vec::new(),
            module_startup_hooks: Vec::new(),
            ini_modify_hooks: std::collections::HashMap::new(),
            module_info: Vec::new(),
            dim_ctx: crate::ext::DimensionCtx::Read,
            prop_ctx: crate::ext::PropCtx::Read,
            ini_stage: crate::ext::IniStage::Runtime,
            magic_call_name: None,
            output_handler_aliases: std::collections::HashMap::new(),
            include_opener: None,
            source_filter: None,
            builtin_deprecations: std::collections::HashMap::new(),
            const_deprecation_guard: std::collections::HashSet::new(),
            const_attributes: std::collections::HashMap::new(),
            const_files: std::collections::HashMap::new(),
            deprecated_constants: std::collections::HashMap::new(),
            pending_call_dst: None,
            empty_code,
            empty_vars: Rc::new(RefCell::new(vm::VarStore::default())),
            prop_guards: HashSet::new(),
        };
        for n in ["error_reporting", "precision", "serialize_precision"] {
            vm.sync_ini(n);
        }
        builtins::register_all(&mut vm);
        vm
    }

    /// Re-reads cached ini values after a change.
    pub(crate) fn sync_ini(&mut self, name: &str) {
        match name {
            "error_reporting" => {
                self.error_reporting = match self.ini.get("error_reporting") {
                    Some(v) if !v.is_empty() => std::str::from_utf8(v)
                        .ok()
                        .and_then(|s| s.trim().parse::<i64>().ok())
                        .unwrap_or(0),
                    _ => 30719,
                };
            }
            "precision" => self.precision = self.ini.get_long("precision"),
            "max_execution_time" => {
                let t = self.ini.get_long("max_execution_time");
                self.set_time_limit(t);
            }
            "memory_limit" | "max_memory_limit" => {
                let limit = u64::try_from(self.ini.get_long("memory_limit")).unwrap_or(u64::MAX);
                php_memlimit::GLOBAL.force_limit(php_memlimit::limit_from_ini(limit));
            }
            "serialize_precision" => {
                self.serialize_precision = self.ini.get_long("serialize_precision")
            }
            _ => {}
        }
    }

    /// Sets `$argv`, `$argc` and `$_SERVER` for a CLI script.
    pub fn set_cli_args(&mut self, script: &str, args: &[Vec<u8>]) {
        let mut argv = vec![Value::Str(ZStr::from(script))];
        argv.extend(args.iter().map(|a| Value::Str(ZStr::from(a.as_slice()))));
        let argc = i64::try_from(argv.len()).unwrap_or(0);
        let argv_arr = Value::Array(ZArray::from_values(argv));
        let mut server = HashTable::new();
        for (k, v) in std::env::vars_os() {
            use std::os::unix::ffi::OsStrExt;
            server.insert(
                ArrayKey::from_bytes(k.as_bytes()),
                Value::Str(ZStr::from(v.as_bytes())),
            );
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let s = |x: &str| Value::Str(ZStr::from(x));
        server.insert(ArrayKey::from("PHP_SELF"), s(script));
        server.insert(ArrayKey::from("SCRIPT_NAME"), s(script));
        server.insert(ArrayKey::from("SCRIPT_FILENAME"), s(script));
        server.insert(ArrayKey::from("PATH_TRANSLATED"), s(script));
        server.insert(ArrayKey::from("DOCUMENT_ROOT"), s(""));
        server.insert(
            ArrayKey::from("REQUEST_TIME_FLOAT"),
            Value::Float(now.as_secs_f64()),
        );
        server.insert(
            ArrayKey::from("REQUEST_TIME"),
            Value::Int(i64::try_from(now.as_secs()).unwrap_or(0)),
        );
        server.insert(ArrayKey::from("argv"), argv_arr.clone());
        server.insert(ArrayKey::from("argc"), Value::Int(argc));
        let mut env = HashTable::new();
        for (k, v) in std::env::vars_os() {
            use std::os::unix::ffi::OsStrExt;
            env.insert(
                ArrayKey::from_bytes(k.as_bytes()),
                Value::Str(ZStr::from(v.as_bytes())),
            );
        }
        let mut g = self.globals.borrow_mut();
        let mut set = |name: &str, v: Value| {
            let i = g.find_or_add(name.as_bytes()) as usize;
            g.slots[i] = Some(v);
        };
        set("_GET", Value::Array(ZArray::new()));
        set("_POST", Value::Array(ZArray::new()));
        set("_COOKIE", Value::Array(ZArray::new()));
        set("_FILES", Value::Array(ZArray::new()));
        set("_ENV", Value::Array(ZArray::from_table(env)));
        set("_REQUEST", Value::Array(ZArray::new()));
        set("_SERVER", Value::Array(ZArray::from_table(server)));
        set("argv", argv_arr);
        set("argc", Value::Int(argc));
        drop(g);
        self.script_path = ZStr::from(script);
    }

    /// Runs a script and performs request shutdown; returns the exit status.
    pub fn run_script(&mut self, src: &[u8], path: &str) -> i32 {
        php_support::output::request_startup(
            self,
            None,
            self.ini.get_long("output_buffering"),
            self.ini.get_bool("implicit_flush"),
        );
        for hook in self.startup_hooks.clone() {
            hook(self);
        }
        // memory_limit accounting starts with the request.
        let limit = u64::try_from(self.ini.get_long("memory_limit")).unwrap_or(u64::MAX);
        if php_memlimit::request_start(php_memlimit::limit_from_ini(limit)).is_err() {
            php_memlimit::GLOBAL.force_limit(Some(php_memlimit::DEFAULT_LIMIT));
        }
        let t = self.ini.get_long("max_execution_time");
        self.set_time_limit(t);
        let p = ZStr::from(path);
        let mut r = Ok(());
        let prepend = self
            .ini
            .get("auto_prepend_file")
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if !prepend.is_empty() {
            r = self.include_file(&String::from_utf8_lossy(&prepend));
        }
        if r.is_ok() {
            r = self.run_main(src, &p);
        }
        if r.is_ok() {
            let append = self
                .ini
                .get("auto_append_file")
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            if !append.is_empty() {
                r = self.include_file(&String::from_utf8_lossy(&append));
            }
        }
        self.shutdown(r)
    }

    /// Request shutdown: shutdown functions, destructors, output flush.
    pub fn shutdown(&mut self, r: Result<(), Throw>) -> i32 {
        let fatal = matches!(r, Err(Throw::Fatal));
        // `oom_after_main_script`: a violation recorded by the last
        // statement is still reported.
        if !fatal && php_memlimit::is_pending() {
            let _ = self.raise_pending_oom();
        }
        self.frames.clear();
        // A shutdown function may raise its own OOM.
        php_memlimit::clear();
        // Shutdown functions.
        let mut i = 0;
        while i < self.shutdown_functions.len() {
            let (cb, args) = self.shutdown_functions[i].clone();
            i += 1;
            match self.call_value(&cb, args) {
                Ok(_) => {}
                Err(Throw::Exception(e)) => {
                    if self.uncaught_exception(e).is_err() {
                        break;
                    }
                }
                Err(Throw::Exit) => break,
                Err(Throw::Fatal | Throw::FiberSuspend) => break,
            }
            self.frames.clear();
        }
        let _ = fatal;
        // Destructors.
        if object::destructors_enabled() {
            self.in_shutdown = true;
            let _ = self.call_global_destructors();
            self.in_shutdown = false;
        }
        object::set_destructors_enabled(false);
        for h in self.shutdown_hooks.clone() {
            h(self);
        }
        php_support::output::request_shutdown(self);
        self.flush_stdout();
        php_memlimit::request_end();
        self.exit_status
    }

    fn call_global_destructors(&mut self) -> Result<(), Throw> {
        // Globals holding the only reference, in reverse order.
        loop {
            let n = self.globals.borrow().slots.len();
            let mut removed = false;
            for i in (0..n).rev() {
                let take = {
                    let g = self.globals.borrow();
                    match &g.slots[i] {
                        Some(Value::Object(o)) => o.refcount() == 1,
                        _ => false,
                    }
                };
                if take {
                    let v = self.globals.borrow_mut().slots[i].take();
                    drop(v);
                    removed = true;
                    if let Err(e) = self.run_pending_destructors() {
                        return self.destructor_error(e);
                    }
                }
            }
            if !removed {
                break;
            }
        }
        // Remaining objects in creation order.
        for d in object::live_objects() {
            if d.flags.get() & object::oflags::DESTRUCTED != 0 || !d.class.has_destructor() {
                continue;
            }
            d.flags.set(d.flags.get() | object::oflags::DESTRUCTED);
            let o = object::revive(d);
            if let Err(e) = self.call_destructor(&o) {
                return self.destructor_error(e);
            }
            if let Err(e) = self.run_pending_destructors() {
                return self.destructor_error(e);
            }
        }
        Ok(())
    }

    fn destructor_error(&mut self, e: Throw) -> Result<(), Throw> {
        match e {
            Throw::Exception(ex) => {
                let _ = self.uncaught_exception(ex);
                object::set_destructors_enabled(false);
                Err(Throw::Fatal)
            }
            other => Err(other),
        }
    }

    /// Checks the syntax of a file (`php -l`). Returns the error message.
    pub fn lint(src: &[u8]) -> Result<(), (String, u32, bool)> {
        let opts = php_parser::ParseOptions {
            skip_shebang: true,
            ..Default::default()
        };
        match php_parser::parse(src, &opts) {
            Ok(_) => Ok(()),
            Err(e) => Err((e.message, e.line, e.kind == php_parser::ErrorKind::Parse)),
        }
    }
}

/// Core constants usable in INI values before any extension starts
/// (`zend_register_standard_constants`).
fn startup_ini_constant(name: &[u8]) -> Option<Vec<u8>> {
    let v: i64 = match name {
        b"E_ERROR" => 1,
        b"E_WARNING" => 2,
        b"E_PARSE" => 4,
        b"E_NOTICE" => 8,
        b"E_CORE_ERROR" => 16,
        b"E_CORE_WARNING" => 32,
        b"E_COMPILE_ERROR" => 64,
        b"E_COMPILE_WARNING" => 128,
        b"E_USER_ERROR" => 256,
        b"E_USER_WARNING" => 512,
        b"E_USER_NOTICE" => 1024,
        b"E_STRICT" => 2048,
        b"E_RECOVERABLE_ERROR" => 4096,
        b"E_DEPRECATED" => 8192,
        b"E_USER_DEPRECATED" => 16384,
        b"E_ALL" => 30719,
        b"PHP_INT_MAX" => i64::MAX,
        b"PHP_INT_SIZE" => 8,
        _ => return None,
    };
    Some(v.to_string().into_bytes())
}

struct DefineCollector(Vec<(String, String)>);

impl php_support::ini::IniParserCallback for DefineCollector {
    fn section(&mut self, _name: &[u8]) {}
    fn entry(&mut self, key: &[u8], value: Option<php_support::ini::IniScalar>) {
        let v = value.map(|v| v.to_bytes()).unwrap_or_default();
        self.0.push((
            String::from_utf8_lossy(key).into_owned(),
            String::from_utf8_lossy(&v).into_owned(),
        ));
    }
    fn pop_entry(&mut self, key: &[u8], value: php_support::ini::IniScalar, _offset: &[u8]) {
        self.entry(key, Some(value));
    }
}

/// The CLI's `-d name=value` (`php_cli.c`): a value starting with a
/// character that is neither alphanumeric nor a quote is quoted, then the
/// line is parsed as INI (comments, constants such as `E_ALL`,
/// expressions, quoting).
pub fn parse_cli_define(arg: &[u8]) -> Vec<(String, String)> {
    let mut line = Vec::with_capacity(arg.len() + 4);
    match arg.iter().position(|&c| c == b'=') {
        Some(p) => {
            let val = &arg[p + 1..];
            match val.first() {
                Some(&c) if !c.is_ascii_alphanumeric() && c != b'"' && c != b'\'' => {
                    line.extend_from_slice(&arg[..=p]);
                    line.push(b'"');
                    line.extend_from_slice(val);
                    line.push(b'"');
                }
                _ => line.extend_from_slice(arg),
            }
        }
        None => {
            line.extend_from_slice(arg);
            line.extend_from_slice(b"=1");
        }
    }
    line.push(b'\n');
    let mut hooks = php_support::ini::FnHooks {
        constant: startup_ini_constant,
        variable: |n: &[u8]| {
            std::env::var_os(String::from_utf8_lossy(n).as_ref())
                .map(|v| v.to_string_lossy().into_owned().into_bytes())
        },
    };
    let mut out = DefineCollector(Vec::new());
    let _ = php_support::ini::parse_ini(
        &line,
        "Unknown",
        php_support::ini::ScannerMode::Normal,
        &mut hooks,
        &mut out,
    );
    out.0
}

#[cfg(test)]
mod cli_define_tests {
    use super::parse_cli_define;

    fn one(s: &str) -> (String, String) {
        parse_cli_define(s.as_bytes())
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    #[test]
    fn defines() {
        assert_eq!(
            one("error_reporting=E_ALL"),
            ("error_reporting".into(), "30719".into())
        );
        assert_eq!(
            one("date.timezone=UTC;"),
            ("date.timezone".into(), "UTC".into())
        );
        assert_eq!(one("display_errors"), ("display_errors".into(), "1".into()));
        assert_eq!(one("x=\"a b\""), ("x".into(), "a b".into()));
        assert_eq!(
            one("error_reporting=E_ALL & ~E_DEPRECATED"),
            ("error_reporting".into(), "22527".into())
        );
    }
}

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
mod configuration;
mod context;
mod enums;
mod errors;
mod exceptions;
mod exec;
pub mod ext;
mod fiber;
pub mod func;
pub mod gc;
mod generator;
mod include;
pub mod interrupt;
mod iter;
mod lazy;
pub mod object;
mod observer;
pub mod opcode;
mod persistence;
mod pfa;
mod place;
mod props;
mod reflection;
mod typecheck;
mod typed_refs;
pub mod types;
mod variance;
pub mod vm;
mod weakref;

pub use exec::{oom_pending, try_reserve_memory};
pub use persistence::PersistentDeclarations;
pub use vm::{ClassLoader, Throw, Vm, VmResult};

use php_runtime::{Value, ZArray, ZStr};
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
        // A new executor (`init_executor()`): a fatal error or the shutdown
        // of an earlier request on this thread disabled destructors.
        object::set_destructors_enabled(true);
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
            configuration: None,
            output: php_support::output::OutputStack::new(),
            stdout,
            error_reporting: 30719,
            error_handlers: Vec::new(),
            exception_handlers: Vec::new(),
            included: Vec::new(),
            included_set: HashSet::new(),
            autoloaders: Vec::new(),
            autoloading: HashSet::new(),
            attribute_call_sites: Vec::new(),
            exit_status: 0,
            shutdown_functions: Vec::new(),
            last_error: None,
            last_fatal_error_backtrace: None,
            pending_throw: None,
            output_handler_threw: false,
            script_path: ZStr::empty(),
            argv: Vec::new(),
            bound_decls: HashMap::new(),
            units: Vec::new(),
            precision: 14,
            serialize_precision: -1,
            in_shutdown: false,
            in_error_handler: 0,
            error_msg_html_ready: false,
            reporting_oom: false,
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
            response_mimetype: None,
            send_default_content_type: true,
            response_header_hook: None,
            rfc1867_callback: None,
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
            startup_diagnostics: Vec::new(),
            log_hook: None,
            main_return: Value::Null,
            native_stack_base: 0,
            native_stack_size: 8 << 20,
            stack_limit_ini: None,
            call_bufs: crate::vm::CallBufs::prewarmed(&empty_code),
            output_handler_aliases: std::collections::HashMap::new(),
            output_handler_conflicts: std::collections::HashMap::new(),
            output_handler_hooks: std::collections::HashMap::new(),
            include_opener: None,
            source_filter: None,
            script_encoding_provider: None,
            parsed_sources: std::cell::RefCell::new(std::collections::HashMap::new()),
            builtin_deprecations: std::collections::HashMap::new(),
            builtin_attributes: std::collections::HashMap::new(),
            builtin_doc_comments: std::collections::HashMap::new(),
            const_deprecation_guard: std::collections::HashSet::new(),
            const_attributes: std::collections::HashMap::new(),
            const_files: std::collections::HashMap::new(),
            deprecated_constants: std::collections::HashMap::new(),
            const_deprecation_filter: std::cell::RefCell::new((0, Vec::new())),
            pending_call_dst: None,
            empty_code,
            empty_vars: Rc::new(RefCell::new(vm::VarStore::default())),
            prop_guards: HashSet::new(),
            observer: crate::observer::ObserverState::default(),
        };
        for n in ["error_reporting", "precision", "serialize_precision"] {
            vm.sync_ini(n);
        }
        vm.startup_diagnostics = sink;
        // `zend_interned_strings_init()`: the empty and one-character
        // strings exist before any script runs, so their first use (e.g.
        // `(string) 0`) does not show in `memory_get_usage()` deltas.
        let _ = (ZStr::empty(), ZStr::from_byte(0));
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

    /// Sets `$argv` and `$argc` for a CLI script (the SAPI fills the
    /// superglobals: `$_SERVER`, `$_ENV`, the request arrays).
    pub fn set_cli_args(&mut self, script: &str, args: &[Vec<u8>]) {
        let mut argv = vec![Value::Str(ZStr::from(script))];
        argv.extend(args.iter().map(|a| Value::Str(ZStr::from(a.as_slice()))));
        let argc = i64::try_from(argv.len()).unwrap_or(0);
        let argv_arr = Value::Array(ZArray::from_values(argv));
        let mut g = self.globals.borrow_mut();
        let mut set = |name: &str, v: Value| {
            let i = g.find_or_add(name.as_bytes()) as usize;
            g.slots[i] = Some(v);
        };
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
        self.observer_unwind(0, r.as_ref().err());
        self.frames.clear();
        // A shutdown function may raise its own OOM.
        Vm::clear_pending_oom();
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
        for h in self.shutdown_hooks.clone() {
            h(self);
        }
        php_support::output::request_shutdown(self);
        object::set_destructors_enabled(false);
        // `sapi_deactivate`: headers go out (and header_register_callback
        // runs) even when the script produced no output.
        crate::builtins::headers::send_headers(self);
        self.flush_stdout();
        php_memlimit::request_end();
        self.exit_status
    }

    fn call_global_destructors(&mut self) -> Result<(), Throw> {
        // Objects released on the way here (the uncaught exception and its
        // trace, the unwound frames).
        if let Err(e) = self.run_pending_destructors() {
            return self.destructor_error(e);
        }
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
        // `zend_objects_store_call_destructors`: the remaining objects in
        // handle order, including the ones their destructors create (which
        // take fresh handles at the end of the store meanwhile).
        object::set_handle_no_reuse(true);
        let r = self.call_store_destructors();
        object::set_handle_no_reuse(false);
        r
    }

    fn call_store_destructors(&mut self) -> Result<(), Throw> {
        // The store is not freed here: an object whose destructor dropped
        // its last reference stays allocated (`GC_DELREF`, no release).
        let mut keep = Vec::new();
        let mut i = 0;
        while i < object::store_len() {
            let slot = object::live_object_at(i);
            i += 1;
            let Some(d) = slot else {
                continue;
            };
            if d.flags.get() & object::oflags::DESTRUCTED != 0 || !d.class.has_destructor() {
                continue;
            }
            d.flags.set(d.flags.get() | object::oflags::DESTRUCTED);
            let o = object::revive(d.clone());
            keep.push(d);
            if let Err(e) = self.call_destructor(&o) {
                return self.destructor_error(e);
            }
            drop(o);
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

    /// Checks file syntax with default settings (legacy embedding convenience).
    /// Configured hosts, including CLI lint, use [`Self::parse_source`] instead.
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
    /// `name[offset]=value` keeps its offset in the name (`ary[a]`) for
    /// the SAPI's configuration hash (`ZEND_INI_PARSER_POP_ENTRY`).
    fn pop_entry(&mut self, key: &[u8], value: php_support::ini::IniScalar, offset: &[u8]) {
        let mut name = key.to_vec();
        name.push(b'[');
        name.extend_from_slice(offset);
        name.push(b']');
        self.entry(&name, Some(value));
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
        assert_eq!(one("ary[a]=1"), ("ary[a]".into(), "1".into()));
        assert_eq!(one("ary[]=x"), ("ary[]".into(), "x".into()));
    }
}

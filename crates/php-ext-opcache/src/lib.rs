//! `ext/opcache` for the phprust VM: binds the `php-opcache` library
//! through the `php_vm::ext` extension API.
//!
//! ref: ext/opcache/zend_accelerator_module.c, ext/opcache/opcache.stub.php

use php_opcache::blacklist::Blacklist;
use php_opcache::file_cache;
use php_opcache::{CacheLookup, PreloadStats, ScriptCache};
pub use php_opcache::{JitBuffer, JitConfig, JitTraceInfo, JitTraceTracker};
use php_runtime::{Value, ZArray, ZStr};
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::compiler::CompiledUnit;
use php_vm::ext::{Args, Extension, InfoItem, ModuleBuilder, Throw, VmResult};
use php_vm::include::CompileOutcome;
use php_vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static GLOBAL_CACHE: RefCell<ScriptCache<Rc<CompiledUnit>>> = RefCell::new(ScriptCache::new(10000));
    /// `accel_blacklist`, with the `opcache.blacklist_filename` it was
    /// loaded from.
    static BLACKLIST: RefCell<Option<(String, Rc<Blacklist>)>> = const { RefCell::new(None) };
    /// `ZCSG(restart_pending)`.
    static RESTART_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// `JIT_G(enabled)`: decided at startup, cleared by a run-time
    /// `opcache.jit=disable`.
    static JIT_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// `accel_blacklist`: loaded once from `opcache.blacklist_filename`
/// (`zend_accel_blacklist_load()` at startup, which logs what it cannot
/// load); `None` when the directive is empty.
fn blacklist(vm: &mut Vm) -> Option<Rc<Blacklist>> {
    let pattern = get_ini_string(vm, "opcache.blacklist_filename");
    if pattern.is_empty() {
        return None;
    }
    let loaded = BLACKLIST.with(|b| {
        b.borrow()
            .as_ref()
            .filter(|(p, _)| *p == pattern)
            .map(|(_, l)| l.clone())
    });
    if loaded.is_some() {
        return loaded;
    }
    let list = Rc::new(Blacklist::load(&pattern));
    for warning in &list.warnings {
        vm_accel_log(vm, AccelLog::Warning, warning);
    }
    BLACKLIST.with(|b| *b.borrow_mut() = Some((pattern, list.clone())));
    Some(list)
}

/// The `opcache` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Opcache;

impl Extension for Opcache {
    fn name(&self) -> &'static str {
        "Zend OPcache"
    }

    fn register(&self, module: &mut ModuleBuilder<'_>) {
        // Also register "opcache" in loaded extensions so tests with `--EXTENSIONS-- opcache` pass
        module.vm().loaded_extensions.push("opcache".to_string());

        // Register INI directives
        register_ini(module);

        // Includes of deleted files served from the cache (`validate_timestamps=0`).
        Vm::set_cached_include_hook(opcache_cached_include);

        // Register functions
        module
            .function_static("opcache_reset(): bool", opcache_reset)
            .function_static(
                "opcache_get_status(bool $include_scripts = true): array|false",
                opcache_get_status,
            )
            .function_static(
                "opcache_compile_file(string $filename): bool",
                opcache_compile_file,
            )
            .function_static(
                "opcache_invalidate(string $filename, bool $force = false): bool",
                opcache_invalidate,
            )
            .function_static(
                "opcache_jit_blacklist(object $closure): void",
                opcache_jit_blacklist,
            )
            .function_static(
                "opcache_get_configuration(): array|false",
                opcache_get_configuration,
            )
            .function_static(
                "opcache_is_script_cached(string $filename): bool",
                opcache_is_script_cached,
            )
            .function_static(
                "opcache_is_script_cached_in_file_cache(string $filename): bool",
                opcache_is_script_cached_in_file_cache,
            );

        module.info(module_info);

        // Install additive compile hook
        Vm::set_compile_hook(opcache_compile_hook);
    }
}

/// The levels of `zend_accel_error()` (`zend_accelerator_debug.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccelLog {
    /// `ACCEL_LOG_FATAL`.
    Fatal = 0,
    /// `ACCEL_LOG_ERROR`.
    Error = 1,
    /// `ACCEL_LOG_WARNING`.
    Warning = 2,
    /// `ACCEL_LOG_INFO`.
    Info = 3,
    /// `ACCEL_LOG_DEBUG`.
    Debug = 4,
}

/// `zend_accel_error()` (`zend_accelerator_debug.c`): when
/// `opcache.log_verbosity_level` (`verbosity`) admits `kind`, writes
/// "<asctime> (<pid>): <Kind> <message>" and a newline to
/// `opcache.error_log` (`error_log`; stderr when empty or "stderr", or
/// when the file cannot be opened). The caller does what the level implies
/// (a fatal exits, an error bails out).
pub fn accel_log(verbosity: i64, error_log: &str, kind: AccelLog, message: &str) {
    use std::io::Write as _;
    if kind as i64 > verbosity {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let label = match kind {
        AccelLog::Fatal => "Fatal Error",
        AccelLog::Error => "Error",
        AccelLog::Warning => "Warning",
        AccelLog::Info => "Message",
        AccelLog::Debug => "Debug",
    };
    let line = format!(
        "{} ({}): {label} {message}\n",
        format_ctime(now),
        std::process::id()
    );
    if !error_log.is_empty() && error_log != "stderr" {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(error_log)
        {
            let _ = f.write_all(line.as_bytes());
            return;
        }
    }
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.flush();
}

/// [`accel_log`] with the request's `opcache.log_verbosity_level` and
/// `opcache.error_log`. The script's output so far goes out first, as
/// php-src's CLI writes it unbuffered.
fn vm_accel_log(vm: &mut Vm, kind: AccelLog, message: &str) {
    let verbosity = vm.ini_long("opcache.log_verbosity_level");
    if kind as i64 > verbosity {
        return;
    }
    vm.flush_stdout();
    accel_log(
        verbosity,
        &get_ini_string(vm, "opcache.error_log"),
        kind,
        message,
    );
}

/// `zend_accel_error(ACCEL_LOG_WARNING, ...)` for the startup INI checks,
/// whose messages end in their own newline, as php-src's do (the line
/// then ends in two).
/// `zend_accel_error_noreturn(ACCEL_LOG_FATAL, ...)`: logged when
/// `opcache.log_verbosity_level` admits fatal errors, and the process
/// exits with status 254 (-2) whether logged or not.
fn accel_fatal(vm: &Vm, message: &str) -> ! {
    accel_log(
        vm.ini_long("opcache.log_verbosity_level"),
        &get_ini_string(vm, "opcache.error_log"),
        AccelLog::Fatal,
        message,
    );
    std::process::exit(254);
}

fn accel_log_warning(vm: &mut Vm, message: &str) {
    vm_accel_log(vm, AccelLog::Warning, message);
}

/// The startup-stage checks of opcache's INI handlers
/// (`OnUpdateMemoryConsumption`, `OnUpdateInternedStringsBuffer`,
/// `OnUpdateMaxAcceleratedFiles`, `OnUpdateMaxWastedPercentage`): a value
/// out of range is logged and the directive keeps its default. Runs once,
/// before the first request.
fn check_startup_ini(vm: &mut Vm) {
    thread_local! {
        static CHECKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if CHECKED.with(|c| c.replace(true)) {
        return;
    }
    /// `MAX_INTERNED_STRINGS_BUFFER_SIZE` on 64-bit builds.
    const MAX_INTERNED_STRINGS_BUFFER_SIZE: i64 = 32767;
    /// `MIN_ACCEL_FILES` / `MAX_ACCEL_FILES`.
    const MIN_ACCEL_FILES: i64 = 200;
    const MAX_ACCEL_FILES: i64 = 1_000_000;
    if vm.ini_long("opcache.memory_consumption") < 8 {
        accel_log_warning(
            vm,
            "opcache.memory_consumption is set below the required 8MB.\n",
        );
        vm.ini.reset_to_default("opcache.memory_consumption");
    }
    let size = vm.ini_long("opcache.interned_strings_buffer");
    if size < 0 {
        accel_log_warning(
            vm,
            &format!("opcache.interned_strings_buffer must be greater than or equal to 0, {size} given.\n"),
        );
    } else if size > MAX_INTERNED_STRINGS_BUFFER_SIZE {
        accel_log_warning(
            vm,
            &format!(
                "opcache.interned_strings_buffer must be less than or equal to {MAX_INTERNED_STRINGS_BUFFER_SIZE}, {size} given.\n"
            ),
        );
    }
    if !(0..=MAX_INTERNED_STRINGS_BUFFER_SIZE).contains(&size) {
        vm.ini.reset_to_default("opcache.interned_strings_buffer");
    }
    let files = vm.ini_long("opcache.max_accelerated_files");
    if files < MIN_ACCEL_FILES {
        accel_log_warning(
            vm,
            &format!("opcache.max_accelerated_files is set below the required minimum ({MIN_ACCEL_FILES}).\n"),
        );
    } else if files > MAX_ACCEL_FILES {
        accel_log_warning(
            vm,
            &format!("opcache.max_accelerated_files is set above the limit ({MAX_ACCEL_FILES}).\n"),
        );
    }
    if !(MIN_ACCEL_FILES..=MAX_ACCEL_FILES).contains(&files) {
        vm.ini.reset_to_default("opcache.max_accelerated_files");
    }
    let wasted = vm.ini_long("opcache.max_wasted_percentage");
    if wasted <= 0 || wasted > 50 {
        accel_log_warning(
            vm,
            "opcache.max_wasted_percentage must be set between 1 and 50.\n",
        );
        vm.ini.reset_to_default("opcache.max_wasted_percentage");
    }
    check_startup_shm(vm);
    JIT_ENABLED.with(|c| c.set(jit_enabled_at_startup(vm)));
}

/// `JIT_G(enabled)` after start-up: `zend_jit_config` on the configured
/// `opcache.jit`, then `accel_post_startup` turns it off without a JIT
/// buffer or in file-cache-only mode (when opcache starts at all).
fn jit_enabled_at_startup(vm: &Vm) -> bool {
    let configured = JitConfig::parse(&get_ini_string(vm, "opcache.jit")).is_ok_and(|c| c.enabled);
    if !configured || !is_opcache_enabled(vm) {
        return configured;
    }
    let raw_buf = vm.ini_string("opcache.jit_buffer_size").unwrap_or_default();
    let (buffer_size, _) = php_support::ini::parse_quantity(&raw_buf);
    buffer_size > 0 && !vm.ini_bool("opcache.file_cache_only")
}

/// `OnUpdateJit` (`zend_jit_config`): at run time the JIT mode can change
/// only while the JIT is enabled.
fn on_update_jit(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    if vm.ini_modify_stage() == php_vm::ext::IniStage::Restore {
        return Ok(true);
    }
    if !JIT_ENABLED.with(std::cell::Cell::get) {
        vm.raise(
            2,
            "Cannot change opcache.jit setting at run-time (JIT is disabled)",
        )?;
        return Ok(false);
    }
    if let Ok(cfg) = JitConfig::parse(&String::from_utf8_lossy(value)) {
        JIT_ENABLED.with(|c| c.set(cfg.enabled));
    }
    Ok(true)
}

/// The shared-memory set-up of `accel_post_startup` /
/// `zend_accel_init_shm`, for its fatal errors: the interned strings
/// buffer must fit in `opcache.memory_consumption`, and file-cache-only
/// mode needs `opcache.file_cache`.
fn check_startup_shm(vm: &Vm) {
    /// `sizeof(zend_accel_shared_globals)` on 64-bit builds.
    const SHARED_GLOBALS_SIZE: i64 = 1088;
    const MIB: i64 = 1024 * 1024;
    if !is_opcache_enabled(vm) {
        return;
    }
    if vm.ini_bool("opcache.file_cache_only") {
        if get_ini_string(vm, "opcache.file_cache").is_empty() {
            accel_fatal(
                vm,
                "opcache.file_cache_only is set without a proper setting of opcache.file_cache",
            );
        }
        return;
    }
    let interned = vm.ini_long("opcache.interned_strings_buffer");
    let size = if interned > 0 {
        SHARED_GLOBALS_SIZE + interned.saturating_mul(MIB)
    } else {
        SHARED_GLOBALS_SIZE + 4
    };
    if size
        > vm.ini_long("opcache.memory_consumption")
            .saturating_mul(MIB)
    {
        accel_fatal(
            vm,
            &format!(
                "Insufficient shared memory for interned strings buffer! (tried to allocate {size} bytes)"
            ),
        );
    }
}

/// `OnEnable` (`zend_accelerator_module.c`): at run time opcache may only be
/// disabled; enabling it again warns and is rejected, unless it is already
/// on (a no-op).
fn on_update_enable(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    if vm.ini_modify_stage() == php_vm::ext::IniStage::Restore
        || !php_support::ini::quantity::parse_bool(value)
        || vm.ini_bool("opcache.enable")
    {
        return Ok(true);
    }
    // `zend_error(E_WARNING, ...)`: no function-name prefix.
    vm.raise(
        2,
        "Zend OPcache can't be temporarily enabled (it may be only disabled until the end of request)",
    )?;
    Ok(false)
}

fn register_ini(m: &mut ModuleBuilder<'_>) {
    let d = |name: &'static str, val: Option<&'static str>, acc: Access, handler: ModifyHandler| {
        IniDirective::new(name, val, acc, handler, "opcache")
    };

    m.ini_on_modify("opcache.enable", on_update_enable);
    m.ini_on_modify("opcache.jit", on_update_jit);
    m.on_request_startup(check_startup_ini);
    m.ini(d(
        "opcache.enable",
        Some("1"),
        Access::ALL,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.enable_cli",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.use_cwd",
        Some("1"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.validate_timestamps",
        Some("1"),
        Access::ALL,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.validate_permission",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.validate_root",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.dups_fix",
        Some("0"),
        Access::ALL,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.revalidate_path",
        Some("0"),
        Access::ALL,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.log_verbosity_level",
        Some("1"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.memory_consumption",
        Some("128"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.interned_strings_buffer",
        Some("8"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.max_accelerated_files",
        Some("10000"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.max_wasted_percentage",
        Some("5"),
        Access::SYSTEM,
        ModifyHandler::Real,
    ))
    .ini(d(
        "opcache.force_restart_timeout",
        Some("180"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.revalidate_freq",
        Some("2"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.preferred_memory_model",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.blacklist_filename",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.max_file_size",
        Some("0"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.error_log",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.protect_memory",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.save_comments",
        Some("1"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.record_warnings",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.enable_file_override",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.optimization_level",
        Some("0x7FFEBFFF"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.lockfile_path",
        Some("/tmp"),
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.file_cache",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.file_cache_only",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.file_cache_consistency_checks",
        Some("1"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.file_update_protection",
        Some("2"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.opt_debug_level",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.restrict_api",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.huge_code_pages",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::Bool,
    ))
    .ini(d(
        "opcache.preload",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.preload_user",
        None,
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.jit",
        Some("tracing"),
        Access::ALL,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.jit_buffer_size",
        Some("0"),
        Access::SYSTEM,
        ModifyHandler::None,
    ))
    .ini(d(
        "opcache.jit_debug",
        Some("0"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_bisect_limit",
        Some("0"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_blacklist_root_trace",
        Some("16"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_blacklist_side_trace",
        Some("8"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_hot_func",
        Some("127"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_hot_loop",
        Some("64"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_hot_return",
        Some("8"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_hot_side_exit",
        Some("8"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_exit_counters",
        Some("8192"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_loop_unrolls",
        Some("8"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_polymorphic_calls",
        Some("2"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_recursive_calls",
        Some("2"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_recursive_returns",
        Some("2"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_root_traces",
        Some("1024"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_max_side_traces",
        Some("128"),
        Access::ALL,
        ModifyHandler::Long,
    ))
    .ini(d(
        "opcache.jit_prof_threshold",
        Some("0.005"),
        Access::ALL,
        ModifyHandler::Real,
    ))
    .ini(d(
        "opcache.jit_max_trace_length",
        Some("1024"),
        Access::ALL,
        ModifyHandler::Long,
    ));
}

/// `zend_finalize_system_id` for the file cache, from the internal
/// function table as it stands after `disable_functions` (GH-17733).
fn ensure_system_id(vm: &Vm) {
    if file_cache::system_id_is_final() {
        return;
    }
    let names = vm
        .functions
        .iter()
        .filter(|(_, f)| matches!(f, php_vm::func::Func::Builtin(_)))
        .map(|(name, _)| name.as_bytes());
    file_cache::set_system_id(file_cache::compute_system_id(names));
}

fn is_opcache_enabled(vm: &Vm) -> bool {
    let enable = vm.ini_bool("opcache.enable");
    let enable_cli = vm.ini_bool("opcache.enable_cli");
    enable && enable_cli
}

fn get_ini_string(vm: &Vm, name: &str) -> String {
    vm.ini_string(name)
        .map(|v| String::from_utf8_lossy(&v).into_owned())
        .unwrap_or_default()
}

fn format_ctime(ts: i64) -> String {
    let sec_per_day = 86400i64;
    let mut day = ts.div_euclid(sec_per_day);
    let sec_of_day = ts.rem_euclid(sec_per_day);
    let hour = sec_of_day / 3600;
    let min = (sec_of_day % 3600) / 60;
    let sec = sec_of_day % 60;
    let wday = (day + 4).rem_euclid(7) as usize;
    let wdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

    day += 719468;
    let era = if day >= 0 { day } else { day - 146096 } / 146097;
    let doe = (day - era * 146097) as u32;
    let yoe = (doe - doe / 1020 + doe / 1461 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let months = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {}",
        wdays[wday], months[m as usize], d, hour, min, sec, y
    )
}

fn module_info(vm: &mut Vm) -> Vec<InfoItem> {
    let row = |a: &str, b: &str| InfoItem::Row(vec![a.to_string(), b.to_string()]);
    let enabled = is_opcache_enabled(vm);
    let file_cache_set = !get_ini_string(vm, "opcache.file_cache").is_empty();
    let raw_buf = vm.ini_string("opcache.jit_buffer_size").unwrap_or_default();
    let (raw_size, _) = php_support::ini::parse_quantity(&raw_buf);
    let jit_setting = get_ini_string(vm, "opcache.jit");
    let jit_setting_trimmed = jit_setting.trim();

    let jit_status_str = if !enabled
        || raw_size <= 0
        || jit_setting_trimmed.eq_ignore_ascii_case("disable")
        || jit_setting_trimmed.is_empty()
    {
        "Disabled"
    } else if let Ok(cfg) = JitConfig::parse(jit_setting_trimmed) {
        if !cfg.enabled {
            "Disabled"
        } else if cfg.on {
            "On"
        } else {
            "Off"
        }
    } else {
        "On"
    };

    vec![
        InfoItem::TableStart,
        InfoItem::Header(vec![
            "Opcode Caching".into(),
            if enabled {
                "Up and Running".into()
            } else {
                "Disabled".into()
            },
        ]),
        row("Optimization", if enabled { "Enabled" } else { "Disabled" }),
        row("SHM Cache", if enabled { "Enabled" } else { "Disabled" }),
        row(
            "File Cache",
            if file_cache_set {
                "Enabled"
            } else {
                "Disabled"
            },
        ),
        row("JIT", jit_status_str),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}

#[allow(dead_code)]
fn parse_jit_mode(s: &str) -> (i64, i64, i64) {
    match JitConfig::parse(s) {
        Ok(cfg) => {
            if cfg.enabled && cfg.on {
                (cfg.kind, cfg.opt_level, cfg.opt_flags)
            } else if cfg.enabled && !cfg.on {
                (0, 0, 0)
            } else {
                (0, 0, 0)
            }
        }
        Err(_) => (5, 4, 6),
    }
}

/// Fills the preload statistics of `opcache_get_status()` from what the
/// startup preload (`accel_preload()`, run once by the SAPI before the
/// first request: [`Vm::run_preload`]) made persistent. The preload script
/// itself never runs in a request.
fn sync_preload_stats() {
    if GLOBAL_CACHE.with(|c| c.borrow().preload_stats.is_some()) {
        return;
    }
    let Some(info) = Vm::preload_info() else {
        return;
    };
    let name = |z: &ZStr| String::from_utf8_lossy(z.as_bytes()).into_owned();
    let preloaded_functions: Vec<String> = info.functions.iter().map(name).collect();
    let preloaded_classes: Vec<String> = info.classes.iter().map(name).collect();
    let preloaded_scripts: Vec<String> = info.scripts.iter().map(name).collect();

    // The preload request compiled the scripts through the compile hook,
    // which cached them (the cache is per thread, as preloading is).
    // Calculate memory consumption
    let base_preload_mem = 16384;
    let script_mem: usize = GLOBAL_CACHE.with(|c| {
        let cache = c.borrow();
        preloaded_scripts
            .iter()
            .filter_map(|s| cache.scripts.get(s).map(|e| e.memory_consumption))
            .sum()
    });
    let func_class_mem = (preloaded_functions.len() + preloaded_classes.len()) * 512;
    let memory_consumption = base_preload_mem + script_mem + func_class_mem;

    let stats = PreloadStats {
        memory_consumption,
        functions: preloaded_functions,
        classes: preloaded_classes,
        scripts: preloaded_scripts,
    };

    GLOBAL_CACHE.with(|c| {
        c.borrow_mut().preload_stats = Some(stats);
    });
}

/// The compile hook overriding `compile_source` for files.
fn opcache_compile_hook(
    vm: &mut Vm,
    src: Rc<[u8]>,
    filename: &ZStr,
    skip_shebang: bool,
) -> VmResult<CompileOutcome> {
    sync_preload_stats();

    let enable = vm.ini_bool("opcache.enable");
    let enable_cli = vm.ini_bool("opcache.enable_cli");
    let file_cache_only = vm.ini_bool("opcache.file_cache_only");
    let file_cache_dir = get_ini_string(vm, "opcache.file_cache");
    ensure_system_id(vm);

    if !enable || (!enable_cli && !file_cache_only) {
        return vm.compile_source_default(src, filename, false, skip_shebang);
    }

    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(filename.as_bytes()));
    let realpath = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let real_str = realpath.to_string_lossy().into_owned();

    // A blacklisted script is compiled, never cached (`persistent_compile_file`).
    if blacklist(vm).is_some_and(|l| l.is_blacklisted(realpath.as_os_str().as_bytes())) {
        return vm.compile_source_default(src, filename, false, skip_shebang);
    }

    let mtime_sec = std::fs::metadata(&realpath)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let update_prot = vm.ini_long("opcache.file_update_protection");
    if update_prot > 0 && (now_sec - mtime_sec) < update_prot {
        return vm.compile_source_default(src, filename, false, skip_shebang);
    }

    let validate_timestamps = vm.ini_bool("opcache.validate_timestamps");
    let revalidate_freq = vm.ini_long("opcache.revalidate_freq");

    // 1. In-memory cache check (unless file_cache_only)
    if !file_cache_only {
        let hit = GLOBAL_CACHE.with(|c| {
            let mut cache = c.borrow_mut();
            match cache.get(
                &real_str,
                mtime_sec,
                validate_timestamps,
                revalidate_freq,
                now_sec,
            ) {
                CacheLookup::Hit(unit) => Some(unit.clone()),
                _ => None,
            }
        });
        if let Some(unit) = hit {
            unit.clear_runtime_caches();
            if vm.ini_bool("opcache.record_warnings") {
                vm.emit_compile_warnings(&unit, filename)?;
            }
            return Ok(CompileOutcome::Ok(unit));
        }
    }

    // 2. File cache check
    if !file_cache_dir.is_empty() {
        let cache_dir = std::path::Path::new(&file_cache_dir);
        let consistency_checks = vm.ini_bool("opcache.file_cache_consistency_checks");
        match file_cache::read_file_cache(
            cache_dir,
            &realpath,
            mtime_sec,
            validate_timestamps,
            consistency_checks,
        ) {
            Ok(Some(cached_src)) => {
                let res =
                    vm.compile_source_default(Rc::from(cached_src), filename, false, skip_shebang)?;
                if let CompileOutcome::Ok(ref unit) = res {
                    unit.clear_runtime_caches();
                    if !file_cache_only {
                        GLOBAL_CACHE.with(|c| {
                            c.borrow_mut().insert(
                                real_str,
                                mtime_sec,
                                src.len() + 1024,
                                now_sec,
                                unit.clone(),
                            );
                        });
                    }
                }
                return Ok(res);
            }
            Err(corrupted_msg) => {
                // zend_accel_error(ACCEL_LOG_WARNING, "...\n").
                accel_log_warning(vm, &format!("{corrupted_msg}\n"));
            }
            Ok(None) => {}
        }
    }

    // 3. Cache Miss: compile source
    let res = vm.compile_source_default(src.clone(), filename, false, skip_shebang)?;
    if let CompileOutcome::Ok(ref unit) = res {
        // `zend_accel_script_optimize` -> `zend_optimize_script`.
        vm.call_registered_optimizer_passes();
        if !file_cache_only {
            // `cache_script_in_shared_memory()`.
            vm_accel_log(vm, AccelLog::Info, &format!("Cached script '{real_str}'"));
            GLOBAL_CACHE.with(|c| {
                c.borrow_mut()
                    .insert(real_str, mtime_sec, src.len() + 1024, now_sec, unit.clone());
            });
        }
        if !file_cache_dir.is_empty() {
            let cache_dir = std::path::Path::new(&file_cache_dir);
            if let Err((msg, _)) =
                file_cache::write_file_cache(cache_dir, &realpath, mtime_sec, &src)
            {
                accel_log_warning(vm, &format!("{msg}\n"));
            }
        }
    }
    Ok(res)
}

/// An include of a file that cannot be opened: `persistent_compile_file`
/// looks the script up by its key before opening the file, so with
/// `opcache.validate_timestamps=0` a cached script runs even after its file
/// was deleted (until `opcache_invalidate()`).
fn opcache_cached_include(vm: &mut Vm, name: &[u8]) -> VmResult<Option<(ZStr, Rc<CompiledUnit>)>> {
    use std::os::unix::ffi::OsStrExt;
    if !is_opcache_enabled(vm)
        || vm.ini_bool("opcache.file_cache_only")
        || vm.ini_bool("opcache.validate_timestamps")
    {
        return Ok(None);
    }
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(name));
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return Ok(None),
        }
    };
    // The file is gone: resolve its directory.
    let key = match (full.parent(), full.file_name()) {
        (Some(dir), Some(base)) => {
            std::fs::canonicalize(dir).map_or(full.clone(), |d| d.join(base))
        }
        _ => full,
    };
    let key = key.to_string_lossy().into_owned();
    let now_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let hit = GLOBAL_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if !cache.is_cached(&key) {
            return None;
        }
        match cache.get(&key, 0, false, 0, now_sec) {
            CacheLookup::Hit(unit) => Some(unit.clone()),
            _ => None,
        }
    });
    let Some(unit) = hit else {
        return Ok(None);
    };
    unit.clear_runtime_caches();
    let filename = ZStr::from(key.as_bytes());
    if vm.ini_bool("opcache.record_warnings") {
        vm.emit_compile_warnings(&unit, &filename)?;
    }
    Ok(Some((filename, unit)))
}

fn opcache_reset(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let enabled = is_opcache_enabled(vm);
    if !enabled || vm.ini_bool("opcache.file_cache_only") {
        return Ok(Value::Bool(false));
    }
    // `zend_accel_schedule_restart(ACCEL_RESTART_USER)`, which does not
    // schedule (or log) a pending restart twice.
    if !RESTART_PENDING.with(|r| r.replace(true)) {
        vm_accel_log(vm, AccelLog::Debug, "Restart Scheduled! Reason: user");
    }
    GLOBAL_CACHE.with(|c| c.borrow_mut().reset());
    Ok(Value::Bool(true))
}

fn opcache_get_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sync_preload_stats();
    let include_scripts = if a.has(0) { a.bool(0) } else { true };
    let enabled = is_opcache_enabled(vm);
    let file_cache_only = vm.ini_bool("opcache.file_cache_only");

    if !enabled {
        return Ok(Value::Bool(false));
    }

    if file_cache_only {
        let mut map = ZArray::new();
        map.make_mut()
            .insert(ZStr::from("opcache_enabled"), Value::Bool(false));
        map.make_mut().insert(
            ZStr::from("file_cache"),
            Value::Str(ZStr::from(get_ini_string(vm, "opcache.file_cache"))),
        );
        map.make_mut()
            .insert(ZStr::from("file_cache_only"), Value::Bool(true));
        return Ok(Value::Array(map));
    }

    GLOBAL_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let mut status = ZArray::new();
        status
            .make_mut()
            .insert(ZStr::from("opcache_enabled"), Value::Bool(true));
        status
            .make_mut()
            .insert(ZStr::from("cache_full"), Value::Bool(false));
        status
            .make_mut()
            .insert(ZStr::from("restart_pending"), Value::Bool(false));
        status
            .make_mut()
            .insert(ZStr::from("restart_in_progress"), Value::Bool(false));

        let memory_consumption_mb = vm.ini_long("opcache.memory_consumption");
        let total_memory = (memory_consumption_mb as usize) * 1024 * 1024;
        let used_memory = cache.total_memory_consumption() + 1024 * 1024;
        let free_memory = total_memory.saturating_sub(used_memory);

        let mut mem = ZArray::new();
        mem.make_mut()
            .insert(ZStr::from("used_memory"), Value::Int(used_memory as i64));
        mem.make_mut()
            .insert(ZStr::from("free_memory"), Value::Int(free_memory as i64));
        mem.make_mut()
            .insert(ZStr::from("wasted_memory"), Value::Int(0));
        mem.make_mut()
            .insert(ZStr::from("current_wasted_percentage"), Value::Float(0.0));
        status
            .make_mut()
            .insert(ZStr::from("memory_usage"), Value::Array(mem));

        let interned_mb = vm.ini_long("opcache.interned_strings_buffer");
        let mut interned = ZArray::new();
        interned.make_mut().insert(
            ZStr::from("buffer_size"),
            Value::Int(interned_mb * 1024 * 1024),
        );
        interned
            .make_mut()
            .insert(ZStr::from("used_memory"), Value::Int(1024));
        interned.make_mut().insert(
            ZStr::from("free_memory"),
            Value::Int(interned_mb * 1024 * 1024 - 1024),
        );
        interned
            .make_mut()
            .insert(ZStr::from("number_of_strings"), Value::Int(100));
        status
            .make_mut()
            .insert(ZStr::from("interned_strings_usage"), Value::Array(interned));

        let mut stats = ZArray::new();
        let num_scripts = if cache.preload_stats.is_some() {
            cache.len() + 1
        } else {
            cache.len()
        };
        stats.make_mut().insert(
            ZStr::from("num_cached_scripts"),
            Value::Int(num_scripts as i64),
        );
        stats.make_mut().insert(
            ZStr::from("num_cached_keys"),
            Value::Int(num_scripts as i64),
        );
        stats
            .make_mut()
            .insert(ZStr::from("max_cached_keys"), Value::Int(16229));
        stats
            .make_mut()
            .insert(ZStr::from("hits"), Value::Int(cache.hits as i64));
        stats
            .make_mut()
            .insert(ZStr::from("start_time"), Value::Int(cache.start_time));
        stats.make_mut().insert(
            ZStr::from("last_restart_time"),
            Value::Int(cache.last_restart_time),
        );
        stats
            .make_mut()
            .insert(ZStr::from("oom_restarts"), Value::Int(0));
        stats
            .make_mut()
            .insert(ZStr::from("hash_restarts"), Value::Int(0));
        stats.make_mut().insert(
            ZStr::from("manual_restarts"),
            Value::Int(cache.manual_restarts as i64),
        );
        stats
            .make_mut()
            .insert(ZStr::from("misses"), Value::Int(cache.misses as i64));
        stats
            .make_mut()
            .insert(ZStr::from("blacklist_misses"), Value::Int(0));
        stats
            .make_mut()
            .insert(ZStr::from("blacklist_miss_ratio"), Value::Float(0.0));
        stats.make_mut().insert(
            ZStr::from("opcache_hit_rate"),
            Value::Float(cache.hit_rate()),
        );
        status
            .make_mut()
            .insert(ZStr::from("opcache_statistics"), Value::Array(stats));

        if let Some(ref pstats) = cache.preload_stats {
            let mut preload_arr = ZArray::new();
            preload_arr.make_mut().insert(
                ZStr::from("memory_consumption"),
                Value::Int(pstats.memory_consumption as i64),
            );
            preload_arr.make_mut().insert(
                ZStr::from("functions"),
                Value::Array(ZArray::from_values(
                    pstats
                        .functions
                        .iter()
                        .map(|f| Value::Str(ZStr::from(f.as_str()))),
                )),
            );
            preload_arr.make_mut().insert(
                ZStr::from("classes"),
                Value::Array(ZArray::from_values(
                    pstats
                        .classes
                        .iter()
                        .map(|c| Value::Str(ZStr::from(c.as_str()))),
                )),
            );
            preload_arr.make_mut().insert(
                ZStr::from("scripts"),
                Value::Array(ZArray::from_values(
                    pstats
                        .scripts
                        .iter()
                        .map(|s| Value::Str(ZStr::from(s.as_str()))),
                )),
            );
            status
                .make_mut()
                .insert(ZStr::from("preload_statistics"), Value::Array(preload_arr));
        }

        if include_scripts {
            let mut scripts_arr = ZArray::new();
            if let Some(ref pstats) = cache.preload_stats {
                let mut p = ZArray::new();
                p.make_mut()
                    .insert(ZStr::from("full_path"), Value::Str(ZStr::from("$PRELOAD$")));
                p.make_mut().insert(ZStr::from("hits"), Value::Int(0));
                p.make_mut().insert(
                    ZStr::from("memory_consumption"),
                    Value::Int(pstats.memory_consumption as i64),
                );
                p.make_mut().insert(
                    ZStr::from("last_used"),
                    Value::Str(ZStr::from(format_ctime(0))),
                );
                p.make_mut()
                    .insert(ZStr::from("last_used_timestamp"), Value::Int(0));
                p.make_mut().insert(ZStr::from("timestamp"), Value::Int(0));
                p.make_mut().insert(ZStr::from("revalidate"), Value::Int(0));
                scripts_arr
                    .make_mut()
                    .insert(ZStr::from("$PRELOAD$"), Value::Array(p));
            }
            for (path, entry) in &cache.scripts {
                let mut s = ZArray::new();
                s.make_mut().insert(
                    ZStr::from("full_path"),
                    Value::Str(ZStr::from(entry.full_path.as_str())),
                );
                s.make_mut()
                    .insert(ZStr::from("hits"), Value::Int(entry.hits as i64));
                s.make_mut().insert(
                    ZStr::from("memory_consumption"),
                    Value::Int(entry.memory_consumption as i64),
                );
                s.make_mut().insert(
                    ZStr::from("last_used"),
                    Value::Str(ZStr::from(format_ctime(entry.last_used))),
                );
                s.make_mut().insert(
                    ZStr::from("last_used_timestamp"),
                    Value::Int(entry.last_used),
                );
                s.make_mut()
                    .insert(ZStr::from("timestamp"), Value::Int(entry.timestamp));
                let reval = entry.last_revalidate + vm.ini_long("opcache.revalidate_freq");
                s.make_mut()
                    .insert(ZStr::from("revalidate"), Value::Int(reval));
                scripts_arr
                    .make_mut()
                    .insert(ZStr::from(path.as_str()), Value::Array(s));
            }
            status
                .make_mut()
                .insert(ZStr::from("scripts"), Value::Array(scripts_arr));
        }

        let raw_buf = vm.ini_string("opcache.jit_buffer_size").unwrap_or_default();
        let (raw_size_signed, _) = php_support::ini::parse_quantity(&raw_buf);
        let raw_size = raw_size_signed.max(0) as usize;
        let jit_setting = get_ini_string(vm, "opcache.jit");
        let jit_setting_trimmed = jit_setting.trim();

        let parsed_jit = JitConfig::parse(jit_setting_trimmed);

        let mut jit = ZArray::new();
        match parsed_jit {
            Ok(cfg) if !cfg.enabled => {
                // "disable": completely disabled in php-src
                jit.make_mut()
                    .insert(ZStr::from("enabled"), Value::Bool(false));
                jit.make_mut().insert(ZStr::from("on"), Value::Bool(false));
                jit.make_mut().insert(ZStr::from("kind"), Value::Int(0));
                jit.make_mut()
                    .insert(ZStr::from("opt_level"), Value::Int(0));
                jit.make_mut()
                    .insert(ZStr::from("opt_flags"), Value::Int(0));
                jit.make_mut()
                    .insert(ZStr::from("buffer_size"), Value::Int(0));
                jit.make_mut()
                    .insert(ZStr::from("buffer_free"), Value::Int(0));
            }
            Ok(cfg) if raw_size == 0 => {
                // Buffer size 0: enabled and on become false, but kind/opt_level/opt_flags reflect configured mode
                jit.make_mut()
                    .insert(ZStr::from("enabled"), Value::Bool(false));
                jit.make_mut().insert(ZStr::from("on"), Value::Bool(false));
                jit.make_mut()
                    .insert(ZStr::from("kind"), Value::Int(cfg.kind));
                jit.make_mut()
                    .insert(ZStr::from("opt_level"), Value::Int(cfg.opt_level));
                jit.make_mut()
                    .insert(ZStr::from("opt_flags"), Value::Int(cfg.opt_flags));
                jit.make_mut()
                    .insert(ZStr::from("buffer_size"), Value::Int(0));
                jit.make_mut()
                    .insert(ZStr::from("buffer_free"), Value::Int(0));
            }
            Ok(cfg) => {
                // Configured with buffer_size > 0
                if cache.jit_buffer.is_none()
                    || cache.jit_buffer.as_ref().map(|b| b.raw_size) != Some(raw_size)
                {
                    let max_root = vm.ini_long("opcache.jit_max_root_traces").max(0) as usize;
                    let max_side = vm.ini_long("opcache.jit_max_side_traces").max(0) as usize;
                    let max_exit = vm.ini_long("opcache.jit_max_exit_counters").max(0) as usize;
                    let max_len = vm.ini_long("opcache.jit_max_trace_length").max(0) as usize;
                    cache.init_jit(raw_size, max_root, max_side, max_exit, max_len);
                }
                let (buf_size, buf_free) = if let Some(ref buf) = cache.jit_buffer {
                    (buf.size() as i64, buf.free() as i64)
                } else {
                    (0, 0)
                };

                jit.make_mut()
                    .insert(ZStr::from("enabled"), Value::Bool(cfg.enabled));
                jit.make_mut().insert(ZStr::from("on"), Value::Bool(cfg.on));
                jit.make_mut()
                    .insert(ZStr::from("kind"), Value::Int(cfg.kind));
                jit.make_mut()
                    .insert(ZStr::from("opt_level"), Value::Int(cfg.opt_level));
                jit.make_mut()
                    .insert(ZStr::from("opt_flags"), Value::Int(cfg.opt_flags));
                jit.make_mut()
                    .insert(ZStr::from("buffer_size"), Value::Int(buf_size));
                jit.make_mut()
                    .insert(ZStr::from("buffer_free"), Value::Int(buf_free));
            }
            Err(_) => {
                // Invalid opcache.jit setting falls back to tracing default
                if raw_size == 0 {
                    jit.make_mut()
                        .insert(ZStr::from("enabled"), Value::Bool(false));
                    jit.make_mut().insert(ZStr::from("on"), Value::Bool(false));
                    jit.make_mut().insert(ZStr::from("kind"), Value::Int(5));
                    jit.make_mut()
                        .insert(ZStr::from("opt_level"), Value::Int(4));
                    jit.make_mut()
                        .insert(ZStr::from("opt_flags"), Value::Int(6));
                    jit.make_mut()
                        .insert(ZStr::from("buffer_size"), Value::Int(0));
                    jit.make_mut()
                        .insert(ZStr::from("buffer_free"), Value::Int(0));
                } else {
                    if cache.jit_buffer.is_none()
                        || cache.jit_buffer.as_ref().map(|b| b.raw_size) != Some(raw_size)
                    {
                        let max_root = vm.ini_long("opcache.jit_max_root_traces").max(0) as usize;
                        let max_side = vm.ini_long("opcache.jit_max_side_traces").max(0) as usize;
                        let max_exit = vm.ini_long("opcache.jit_max_exit_counters").max(0) as usize;
                        let max_len = vm.ini_long("opcache.jit_max_trace_length").max(0) as usize;
                        cache.init_jit(raw_size, max_root, max_side, max_exit, max_len);
                    }
                    let (buf_size, buf_free) = if let Some(ref buf) = cache.jit_buffer {
                        (buf.size() as i64, buf.free() as i64)
                    } else {
                        (0, 0)
                    };

                    jit.make_mut()
                        .insert(ZStr::from("enabled"), Value::Bool(true));
                    jit.make_mut().insert(ZStr::from("on"), Value::Bool(true));
                    jit.make_mut().insert(ZStr::from("kind"), Value::Int(5));
                    jit.make_mut()
                        .insert(ZStr::from("opt_level"), Value::Int(4));
                    jit.make_mut()
                        .insert(ZStr::from("opt_flags"), Value::Int(6));
                    jit.make_mut()
                        .insert(ZStr::from("buffer_size"), Value::Int(buf_size));
                    jit.make_mut()
                        .insert(ZStr::from("buffer_free"), Value::Int(buf_free));
                }
            }
        }
        status
            .make_mut()
            .insert(ZStr::from("jit"), Value::Array(jit));

        Ok(Value::Array(status))
    })
}

fn opcache_compile_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sync_preload_stats();
    let filename_z = a.str(0);

    let enabled = vm.ini_bool("opcache.enable")
        && (vm.ini_bool("opcache.enable_cli") || vm.ini_bool("opcache.file_cache_only"));
    if !enabled {
        let (file, line) = vm.current_location();
        let file_s = file.to_string_lossy();
        vm.error(
            8,
            &format!(
                "Zend OPcache has not been properly started, can't compile file in {file_s} on line {line}"
            ),
        )?;
        return Ok(Value::Bool(false));
    }

    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(filename_z.as_bytes()));
    let realpath = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => return Ok(Value::Bool(false)),
    };
    let src = match std::fs::read(&realpath) {
        Ok(s) => s,
        Err(_) => return Ok(Value::Bool(false)),
    };

    let mtime_sec = std::fs::metadata(&realpath)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let update_prot = vm.ini_long("opcache.file_update_protection");
    if update_prot > 0 && (now_sec - mtime_sec) < update_prot {
        return Ok(Value::Bool(false));
    }

    let real_z = ZStr::from(realpath.as_os_str().as_bytes());
    let res = vm.compile_source_default(Rc::from(src.clone()), &real_z, false, true)?;
    match res {
        CompileOutcome::Ok(unit) => {
            let file_cache_only = vm.ini_bool("opcache.file_cache_only");
            if !file_cache_only {
                let real_str = realpath.to_string_lossy().into_owned();
                GLOBAL_CACHE.with(|c| {
                    c.borrow_mut()
                        .insert(real_str, mtime_sec, src.len() + 1024, now_sec, unit);
                });
            }
            let file_cache_dir = get_ini_string(vm, "opcache.file_cache");
            ensure_system_id(vm);
            if !file_cache_dir.is_empty() {
                let cache_dir = std::path::Path::new(&file_cache_dir);
                let _ = file_cache::write_file_cache(cache_dir, &realpath, mtime_sec, &src);
            }
            Ok(Value::Bool(true))
        }
        CompileOutcome::ParseError(e) => Err(Throw::Exception(e)),
        CompileOutcome::Fatal => Ok(Value::Bool(false)),
    }
}

fn opcache_invalidate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let filename_z = a.str(0);
    let force = if a.has(1) { a.bool(1) } else { false };
    let enabled = is_opcache_enabled(vm);
    if !enabled || vm.ini_bool("opcache.file_cache_only") {
        return Ok(Value::Bool(false));
    }

    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(filename_z.as_bytes()));
    let (realpath, file_found) = match std::fs::canonicalize(path) {
        Ok(p) => (p, true),
        Err(_) => (path.to_path_buf(), false),
    };
    let real_str = realpath.to_string_lossy().into_owned();

    let file_cache_dir = get_ini_string(vm, "opcache.file_cache");

    ensure_system_id(vm);
    if !file_cache_dir.is_empty() {
        let cache_dir = std::path::Path::new(&file_cache_dir);
        file_cache::invalidate_file_cache(cache_dir, &realpath);
    }

    let validate_timestamps = vm.ini_bool("opcache.validate_timestamps");
    let mtime_sec = std::fs::metadata(&realpath)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);

    let was_cached = GLOBAL_CACHE.with(|c| {
        c.borrow_mut()
            .invalidate(&real_str, force, mtime_sec, validate_timestamps)
    });

    Ok(Value::Bool(file_found || was_cached))
}

fn opcache_jit_blacklist(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let _ = a.opt(0);
    Ok(Value::Null)
}

fn opcache_get_configuration(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut config = ZArray::new();

    let mut dirs = ZArray::new();
    dirs.make_mut().insert(
        ZStr::from("opcache.enable"),
        Value::Bool(vm.ini_bool("opcache.enable")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.enable_cli"),
        Value::Bool(vm.ini_bool("opcache.enable_cli")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.use_cwd"),
        Value::Bool(vm.ini_bool("opcache.use_cwd")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.validate_timestamps"),
        Value::Bool(vm.ini_bool("opcache.validate_timestamps")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.validate_permission"),
        Value::Bool(vm.ini_bool("opcache.validate_permission")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.validate_root"),
        Value::Bool(vm.ini_bool("opcache.validate_root")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.dups_fix"),
        Value::Bool(vm.ini_bool("opcache.dups_fix")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.revalidate_path"),
        Value::Bool(vm.ini_bool("opcache.revalidate_path")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.log_verbosity_level"),
        Value::Int(vm.ini_long("opcache.log_verbosity_level")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.memory_consumption"),
        Value::Int(vm.ini_long("opcache.memory_consumption") * 1024 * 1024),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.interned_strings_buffer"),
        Value::Int(vm.ini_long("opcache.interned_strings_buffer")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.max_accelerated_files"),
        Value::Int(vm.ini_long("opcache.max_accelerated_files")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.max_wasted_percentage"),
        Value::Float(0.05),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.force_restart_timeout"),
        Value::Int(vm.ini_long("opcache.force_restart_timeout")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.revalidate_freq"),
        Value::Int(vm.ini_long("opcache.revalidate_freq")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.preferred_memory_model"),
        Value::Str(ZStr::from(get_ini_string(
            vm,
            "opcache.preferred_memory_model",
        ))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.blacklist_filename"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.blacklist_filename"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.max_file_size"),
        Value::Int(vm.ini_long("opcache.max_file_size")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.error_log"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.error_log"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.protect_memory"),
        Value::Bool(vm.ini_bool("opcache.protect_memory")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.save_comments"),
        Value::Bool(vm.ini_bool("opcache.save_comments")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.record_warnings"),
        Value::Bool(vm.ini_bool("opcache.record_warnings")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.enable_file_override"),
        Value::Bool(vm.ini_bool("opcache.enable_file_override")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.optimization_level"),
        Value::Int(2147401727),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.lockfile_path"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.lockfile_path"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.file_cache"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.file_cache"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.file_cache_only"),
        Value::Bool(vm.ini_bool("opcache.file_cache_only")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.file_cache_consistency_checks"),
        Value::Bool(vm.ini_bool("opcache.file_cache_consistency_checks")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.file_update_protection"),
        Value::Int(vm.ini_long("opcache.file_update_protection")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.opt_debug_level"),
        Value::Int(vm.ini_long("opcache.opt_debug_level")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.restrict_api"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.restrict_api"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.huge_code_pages"),
        Value::Bool(vm.ini_bool("opcache.huge_code_pages")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.preload"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.preload"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.preload_user"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.preload_user"))),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit"),
        Value::Str(ZStr::from(get_ini_string(vm, "opcache.jit"))),
    );
    let jit_buf_raw = vm.ini_string("opcache.jit_buffer_size").unwrap_or_default();
    let (jit_buf_bytes, _) = php_support::ini::parse_quantity(&jit_buf_raw);
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_buffer_size"),
        Value::Int(jit_buf_bytes),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_debug"),
        Value::Int(vm.ini_long("opcache.jit_debug")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_bisect_limit"),
        Value::Int(vm.ini_long("opcache.jit_bisect_limit")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_blacklist_root_trace"),
        Value::Int(vm.ini_long("opcache.jit_blacklist_root_trace")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_blacklist_side_trace"),
        Value::Int(vm.ini_long("opcache.jit_blacklist_side_trace")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_hot_func"),
        Value::Int(vm.ini_long("opcache.jit_hot_func")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_hot_loop"),
        Value::Int(vm.ini_long("opcache.jit_hot_loop")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_hot_return"),
        Value::Int(vm.ini_long("opcache.jit_hot_return")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_hot_side_exit"),
        Value::Int(vm.ini_long("opcache.jit_hot_side_exit")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_exit_counters"),
        Value::Int(vm.ini_long("opcache.jit_max_exit_counters")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_loop_unrolls"),
        Value::Int(vm.ini_long("opcache.jit_max_loop_unrolls")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_polymorphic_calls"),
        Value::Int(vm.ini_long("opcache.jit_max_polymorphic_calls")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_recursive_calls"),
        Value::Int(vm.ini_long("opcache.jit_max_recursive_calls")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_recursive_returns"),
        Value::Int(vm.ini_long("opcache.jit_max_recursive_returns")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_root_traces"),
        Value::Int(vm.ini_long("opcache.jit_max_root_traces")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_side_traces"),
        Value::Int(vm.ini_long("opcache.jit_max_side_traces")),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_prof_threshold"),
        Value::Float(
            vm.ini_string("opcache.jit_prof_threshold")
                .and_then(|v| std::str::from_utf8(&v).ok()?.trim().parse::<f64>().ok())
                .unwrap_or(0.005),
        ),
    );
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_max_trace_length"),
        Value::Int(vm.ini_long("opcache.jit_max_trace_length")),
    );
    config
        .make_mut()
        .insert(ZStr::from("directives"), Value::Array(dirs));

    let mut ver = ZArray::new();
    ver.make_mut().insert(
        ZStr::from("version"),
        Value::Str(ZStr::from(php_vm::PHP_VERSION)),
    );
    ver.make_mut().insert(
        ZStr::from("opcache_product_name"),
        Value::Str(ZStr::from("Zend OPcache")),
    );
    config
        .make_mut()
        .insert(ZStr::from("version"), Value::Array(ver));

    // `add_blacklist_path` over `accel_blacklist`.
    let mut entries = ZArray::new();
    if let Some(list) = blacklist(vm) {
        for entry in &list.entries {
            let _ = entries
                .make_mut()
                .push(Value::Str(ZStr::from(entry.as_str())));
        }
    }
    config
        .make_mut()
        .insert(ZStr::from("blacklist"), Value::Array(entries));

    Ok(Value::Array(config))
}

fn opcache_is_script_cached(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sync_preload_stats();
    let filename_z = a.str(0);
    let enabled = is_opcache_enabled(vm);
    if !enabled || vm.ini_bool("opcache.file_cache_only") {
        return Ok(Value::Bool(false));
    }

    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(filename_z.as_bytes()));
    let realpath = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => return Ok(Value::Bool(false)),
    };
    let real_str = realpath.to_string_lossy().into_owned();
    let cached = GLOBAL_CACHE.with(|c| c.borrow().is_cached(&real_str));
    Ok(Value::Bool(cached))
}

fn opcache_is_script_cached_in_file_cache(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let filename_z = a.str(0);
    let enabled = vm.ini_bool("opcache.enable")
        && (vm.ini_bool("opcache.enable_cli") || vm.ini_bool("opcache.file_cache_only"));
    let file_cache_dir = get_ini_string(vm, "opcache.file_cache");
    ensure_system_id(vm);
    if !enabled || file_cache_dir.is_empty() {
        return Ok(Value::Bool(false));
    }

    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(filename_z.as_bytes()));
    let realpath = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => return Ok(Value::Bool(false)),
    };
    let validate_timestamps = vm.ini_bool("opcache.validate_timestamps");
    let mtime_sec = std::fs::metadata(&realpath)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let cache_dir = std::path::Path::new(&file_cache_dir);
    let cached = file_cache::is_script_cached_in_file_cache(
        cache_dir,
        &realpath,
        mtime_sec,
        validate_timestamps,
    );
    Ok(Value::Bool(cached))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jit_mode() {
        assert_eq!(parse_jit_mode("function"), (0, 5, 6));
        assert_eq!(parse_jit_mode("tracing"), (5, 4, 6));
        assert_eq!(parse_jit_mode("on"), (5, 4, 6));
        assert_eq!(parse_jit_mode("off"), (0, 0, 0));
        assert_eq!(parse_jit_mode("disable"), (0, 0, 0));
        assert_eq!(parse_jit_mode("0000"), (0, 0, 0));
        assert_eq!(parse_jit_mode("1254"), (5, 4, 6));
        assert_eq!(parse_jit_mode("0254"), (5, 4, 2));
        assert_eq!(parse_jit_mode("1154"), (5, 4, 5));
        assert_eq!(parse_jit_mode("1054"), (5, 4, 4));
        assert_eq!(parse_jit_mode("1205"), (0, 5, 6));
        assert_eq!(parse_jit_mode("1201"), (0, 1, 6));
        assert_eq!(parse_jit_mode("1235"), (3, 5, 6));
        assert_eq!(parse_jit_mode("254"), (5, 4, 2));
        assert_eq!(parse_jit_mode("invalid_mode"), (5, 4, 6));
    }

    #[test]
    fn accel_log_levels_and_format() {
        let path = std::env::temp_dir().join(format!("phprust-accel-log-{}", std::process::id()));
        let log = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        accel_log(2, &log, AccelLog::Info, "not written");
        accel_log(4, &log, AccelLog::Info, "Cached script '/x.php'");
        accel_log(4, &log, AccelLog::Debug, "Restart Scheduled! Reason: user");
        accel_log(
            1,
            &log,
            AccelLog::Fatal,
            "Preloading failed to getpwnam(\"nobody-x\")",
        );
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 3);
        let pid = format!(" ({}): ", std::process::id());
        assert!(lines[0].contains(&pid) && lines[0].ends_with("Message Cached script '/x.php'"));
        assert!(lines[1].ends_with("Debug Restart Scheduled! Reason: user"));
        assert!(lines[2].ends_with("Fatal Error Preloading failed to getpwnam(\"nobody-x\")"));
        // asctime(): "Www Mmm dd hh:mm:ss yyyy".
        assert_eq!(lines[0].find(" (").map(|i| i == 24), Some(true));
    }
}

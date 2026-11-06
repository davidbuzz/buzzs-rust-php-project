//! `ext/opcache` for the phprust VM: binds the `php-opcache` library
//! through the `php_vm::ext` extension API.
//!
//! ref: ext/opcache/zend_accelerator_module.c, ext/opcache/opcache.stub.php

use php_opcache::file_cache;
use php_opcache::{CacheLookup, ScriptCache};
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

        // Register functions
        module
            .function("opcache_reset(): bool", opcache_reset)
            .function(
                "opcache_get_status(bool $include_scripts = true): array|false",
                opcache_get_status,
            )
            .function(
                "opcache_compile_file(string $filename): bool",
                opcache_compile_file,
            )
            .function(
                "opcache_invalidate(string $filename, bool $force = false): bool",
                opcache_invalidate,
            )
            .function(
                "opcache_jit_blacklist(object $closure): void",
                opcache_jit_blacklist,
            )
            .function(
                "opcache_get_configuration(): array|false",
                opcache_get_configuration,
            )
            .function(
                "opcache_is_script_cached(string $filename): bool",
                opcache_is_script_cached,
            )
            .function(
                "opcache_is_script_cached_in_file_cache(string $filename): bool",
                opcache_is_script_cached_in_file_cache,
            );

        module.info(module_info);

        // Install additive compile hook
        Vm::set_compile_hook(opcache_compile_hook);
    }
}

fn register_ini(m: &mut ModuleBuilder<'_>) {
    let d = |name: &'static str, val: Option<&'static str>, acc: Access, handler: ModifyHandler| {
        IniDirective::new(name, val, acc, handler, "opcache")
    };

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
        row("JIT", "Disabled"),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}

/// The compile hook overriding `compile_source` for files.
fn opcache_compile_hook(
    vm: &mut Vm,
    src: Rc<[u8]>,
    filename: &ZStr,
    skip_shebang: bool,
) -> VmResult<CompileOutcome> {
    let enable = vm.ini_bool("opcache.enable");
    let enable_cli = vm.ini_bool("opcache.enable_cli");
    let file_cache_only = vm.ini_bool("opcache.file_cache_only");
    let file_cache_dir = get_ini_string(vm, "opcache.file_cache");

    if !enable || (!enable_cli && !file_cache_only) {
        return vm.compile_source_default(src, filename, false, skip_shebang);
    }

    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(filename.as_bytes()));
    let realpath = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let real_str = realpath.to_string_lossy().into_owned();

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
                for (level, msg, line) in &unit.warnings {
                    vm.error_at(*level, msg, filename, *line)?;
                }
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
                if vm.ini_long("opcache.log_verbosity_level") >= 2 {
                    let _ = vm.error(2, &corrupted_msg);
                }
            }
            Ok(None) => {}
        }
    }

    // 3. Cache Miss: compile source
    let res = vm.compile_source_default(src.clone(), filename, false, skip_shebang)?;
    if let CompileOutcome::Ok(ref unit) = res {
        if !file_cache_only {
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
                if vm.ini_long("opcache.log_verbosity_level") >= 2 {
                    let _ = vm.error(2, &msg);
                }
            }
        }
    }
    Ok(res)
}

fn opcache_reset(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let enabled = is_opcache_enabled(vm);
    if !enabled || vm.ini_bool("opcache.file_cache_only") {
        return Ok(Value::Bool(false));
    }
    GLOBAL_CACHE.with(|c| c.borrow_mut().reset());
    Ok(Value::Bool(true))
}

fn opcache_get_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
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
        let cache = c.borrow();
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
        stats.make_mut().insert(
            ZStr::from("num_cached_scripts"),
            Value::Int(cache.len() as i64),
        );
        stats.make_mut().insert(
            ZStr::from("num_cached_keys"),
            Value::Int(cache.len() as i64),
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

        if include_scripts {
            let mut scripts_arr = ZArray::new();
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

        let mut jit = ZArray::new();
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
        status
            .make_mut()
            .insert(ZStr::from("jit"), Value::Array(jit));

        Ok(Value::Array(status))
    })
}

fn opcache_compile_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
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
    dirs.make_mut().insert(
        ZStr::from("opcache.jit_buffer_size"),
        Value::Int(vm.ini_long("opcache.jit_buffer_size")),
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
        Value::Float(0.005),
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

    config
        .make_mut()
        .insert(ZStr::from("blacklist"), Value::Array(ZArray::new()));

    Ok(Value::Array(config))
}

fn opcache_is_script_cached(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
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

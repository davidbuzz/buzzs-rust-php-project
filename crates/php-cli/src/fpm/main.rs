// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `php-fpm` binary: the `FastCGI` process manager (ref:
//! `sapi/fpm/fpm/fpm_main.c`, `fpm.c`, `fpm_stdio.c`, `fpm_unix.c`,
//! `fpm_env.c`, `fpm_php.c`, `fpm_log.c`).
//!
//! `fpm_main.c` is a fork of `cgi_main.c` over the same `main/fastcgi.c`:
//! the `FastCGI` protocol (`cgi/fastcgi.rs`), the CGI environment
//! (`cgi/env.rs`) and the request (`cgi/request.rs`) are `php-cgi`'s. FPM
//! adds its configuration (`fpm/conf.rs`), the master process and its
//! workers (`fpm/master.rs`), the scoreboard, the status and ping pages,
//! and the access log.

#[allow(dead_code)]
#[path = "../config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../cgi/env.rs"]
mod env;
#[allow(dead_code)]
#[path = "../extensions.rs"]
mod extensions;
#[allow(dead_code)]
#[path = "../cgi/fastcgi.rs"]
mod fastcgi;
#[allow(dead_code)]
#[path = "../getopt.rs"]
mod getopt;
#[path = "../main_stack.rs"]
mod main_stack;
#[allow(dead_code)]
#[path = "../memcap.rs"]
mod memcap;
#[allow(dead_code)]
#[path = "../cgi/request.rs"]
mod request;
#[path = "../startup.rs"]
mod startup;
#[allow(dead_code)]
#[path = "../cgi/sys.rs"]
mod sys;

mod access_log;
mod conf;
mod log;
mod master;
#[allow(dead_code)]
mod os;
mod request_info;
mod scoreboard;
mod sockets;
mod status;

use config::Config;
use getopt::{GetOpt, Opt, INVALID};
#[cfg(test)]
use request::new_vm;
use std::fmt::Write as _;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::rc::Rc;

/// `PHP_CLI_BUILD_DATE` (shared with the `php` binary's build script).
const BUILD_DATE: &str = env!("PHP_CLI_BUILD_DATE");

/// The option table (`OPTIONS[]` of `fpm_main.c`).
const OPTIONS: &[Opt] = &[
    Opt {
        ch: b'c',
        need_param: 1,
        name: "php-ini",
    },
    Opt {
        ch: b'd',
        need_param: 1,
        name: "define",
    },
    Opt {
        ch: b'e',
        need_param: 0,
        name: "profile-info",
    },
    Opt {
        ch: b'h',
        need_param: 0,
        name: "help",
    },
    Opt {
        ch: b'i',
        need_param: 0,
        name: "info",
    },
    Opt {
        ch: b'm',
        need_param: 0,
        name: "modules",
    },
    Opt {
        ch: b'n',
        need_param: 0,
        name: "no-php-ini",
    },
    Opt {
        ch: b'?',
        need_param: 0,
        name: "usage",
    },
    Opt {
        ch: b'v',
        need_param: 0,
        name: "version",
    },
    Opt {
        ch: b'y',
        need_param: 1,
        name: "fpm-config",
    },
    Opt {
        ch: b't',
        need_param: 0,
        name: "test",
    },
    Opt {
        ch: b'p',
        need_param: 1,
        name: "prefix",
    },
    Opt {
        ch: b'g',
        need_param: 1,
        name: "pid",
    },
    Opt {
        ch: b'R',
        need_param: 0,
        name: "allow-to-run-as-root",
    },
    Opt {
        ch: b'D',
        need_param: 0,
        name: "daemonize",
    },
    Opt {
        ch: b'F',
        need_param: 0,
        name: "nodaemonize",
    },
    Opt {
        ch: b'O',
        need_param: 0,
        name: "force-stderr",
    },
];

/// `FPM_EXIT_USAGE`.
const EXIT_USAGE: i32 = 64;

/// `php_cgi_usage()` of `fpm_main.c`.
fn usage(argv0: &[u8]) -> String {
    let a = String::from_utf8_lossy(argv0);
    let prog = a
        .rsplit('/')
        .next()
        .filter(|_| a.contains('/'))
        .unwrap_or("php");
    format!(
        "Usage: {prog} [-n] [-e] [-h] [-i] [-m] [-v] [-t] [-p <prefix>] [-g <pid>] [-c <file>] [-d foo[=bar]] [-y <file>] [-D] [-F [-O]]\n\
  -c <path>|<file> Look for php.ini file in this directory\n\
  -n               No php.ini file will be used\n\
  -d foo[=bar]     Define INI entry foo with value 'bar'\n\
  -e               Generate extended information for debugger/profiler\n\
  -h               This help\n\
  -i               PHP information\n\
  -m               Show compiled in modules\n\
  -v               Version number\n\
  -p, --prefix <dir>\n\
                   Specify alternative prefix path to FastCGI process manager (default: {}).\n\
  -g, --pid <file>\n\
                   Specify the PID file location.\n\
  -y, --fpm-config <file>\n\
                   Specify alternative path to FastCGI process manager config file.\n\
  -t, --test       Test FPM configuration and exit\n\
  -D, --daemonize  force to run in background, and ignore daemonize option from config file\n\
  -F, --nodaemonize\n\
                   force to stay in foreground, and ignore daemonize option from config file\n\
  -O, --force-stderr\n\
                   force output to stderr in nodaemonize even if stderr is not a TTY\n\
  -R, --allow-to-run-as-root\n\
                   Allow pool to run as root (disabled by default)\n",
        conf::PHP_PREFIX
    )
}

fn version_banner() -> String {
    format!(
        "PHP {} (fpm-fcgi) (built: {BUILD_DATE}) (NTS)\nCopyright \u{a9} The PHP Group and Contributors\nZend Engine v{}, Copyright \u{a9} Zend by Perforce\n",
        php_vm::PHP_VERSION,
        php_vm::ZEND_VERSION
    )
}

// The request heap: without it a worker counted nothing, so
// `memory_get_usage()` stayed at its start value and `memory_limit` held
// only where a builtin checks a size before allocating.
php_memlimit::install_global_allocator!();

fn hard_limit(e: php_memlimit::HardLimitExceeded) -> ! {
    let _ = std::io::stderr().write_all(format!("{e} in Unknown on line 0\n").as_bytes());
    // SAFETY: `_exit` ends the process at once; nothing relies on
    // destructors or atexit handlers (PHP does the same).
    unsafe { libc::_exit(137) }
}

fn main() {
    let argv: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
    let on_main_thread = main_stack::ensure(&argv);
    // `memory_limit` accounting and the address-space cap, as in the
    // `php` and `php-cgi` binaries.
    memcap::limit_memory();
    php_memlimit::set_hard_limit_handler(hard_limit);
    php_memlimit::set_tracked(php_memlimit::tracked_from_env(
        std::env::var_os("USE_ZEND_ALLOC")
            .as_deref()
            .map(std::os::unix::ffi::OsStrExt::as_bytes),
        std::env::var_os("USE_TRACKED_ALLOC")
            .as_deref()
            .map(std::os::unix::ffi::OsStrExt::as_bytes),
    ));
    // SAFETY: SIGPIPE is ignored as in fpm_main.c, so writes to a closed
    // socket fail with EPIPE instead of killing the process.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // `fpm_signals_init_mask()`, `fpm_signals_block()`: until the master's
    // handlers are in place (the VM thread inherits this mask).
    os::block_signals(&os::BLOCK_SIGNALS);
    // The VM runs on the main thread, as in C: the master is
    // single-threaded when it forks its workers. Every process (the
    // master and each worker) ends inside `run` with `finish`.
    if on_main_thread {
        finish(run(&argv));
    }
    // Fallback: a thread with a large stack. A worker forked from it has
    // no main thread to return to, so it ends inside `run` as well.
    let _ = std::thread::Builder::new()
        .stack_size(usize::try_from(request::VM_STACK).unwrap_or(usize::MAX))
        .spawn(move || finish(run(&argv)))
        .ok()
        .and_then(|h| h.join().ok());
    finish(70);
}

/// Flushes the standard streams and ends the process with `status`.
fn finish(status: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(status)
}

fn out(bytes: &[u8]) {
    let mut o = std::io::stdout();
    let _ = o.write_all(bytes);
    let _ = o.flush();
}

/// The command line.
#[derive(Default)]
struct Args {
    opts: config::Options,
    fpm_config: Option<String>,
    prefix: Option<String>,
    pid: Option<String>,
    test: u32,
    run_as_root: bool,
    force_daemon: Option<bool>,
    force_stderr: bool,
    info: bool,
}

/// `main()` of `fpm_main.c`.
fn run(argv: &[Vec<u8>]) -> i32 {
    php_memlimit::bind_to_current_thread();
    let argv0 = argv.first().cloned().unwrap_or_else(|| b"php-fpm".to_vec());
    let mut a = Args {
        opts: config::Options {
            binary: std::env::current_exe().ok(),
            cgi: true,
            ..config::Options::default()
        },
        ..Args::default()
    };
    let mut g = GetOpt::new(argv, OPTIONS, false);
    let mut action: Option<u8> = None;
    let s = |v: Option<Vec<u8>>| v.map(|v| String::from_utf8_lossy(&v).into_owned());
    while let Some((c, v)) = g.next_opt() {
        if c == INVALID {
            action = Some(b'!');
            break;
        }
        match u8::try_from(c).unwrap_or(0) {
            b'c' => a.opts.path_override = v,
            b'n' => a.opts.ignore = true,
            b'd' => a
                .opts
                .defines
                .extend(php_vm::parse_cli_define(&v.unwrap_or_default())),
            b'y' => a.fpm_config = s(v),
            b'p' => a.prefix = s(v),
            b'g' => a.pid = s(v),
            b'e' => {}
            b't' => a.test += 1,
            b'i' => a.info = true,
            b'R' => a.run_as_root = true,
            b'D' => a.force_daemon = Some(true),
            b'F' => a.force_daemon = Some(false),
            b'O' => a.force_stderr = true,
            b'm' => {
                action = Some(b'm');
                break;
            }
            b'v' => {
                action = Some(b'v');
                break;
            }
            _ => {
                action = Some(b'h');
                break;
            }
        }
    }
    if action.is_none() && g.optind != argv.len() {
        // "No other args are permitted here as there is no interactive mode".
        action = Some(b'!');
    }
    request::set_fpm();
    // `fcgi_set_logger(fpm_fcgi_log)`: `vzlog("", 0, type, ...)`.
    fastcgi::set_logger(|level, msg| {
        let level = match level {
            2 => log::Level::Notice,
            3 => log::Level::Warning,
            _ => log::Level::Error,
        };
        log::log(level, "", 0, msg, None);
    });
    let cfg = Config::load(&a.opts);
    if let Err(status) = startup::module_startup(&cfg) {
        return status;
    }
    match action {
        Some(b'v') => {
            out(version_banner().as_bytes());
            return 0;
        }
        Some(b'h') => {
            out(usage(&argv0).as_bytes());
            return 0;
        }
        Some(b'!') => {
            out(usage(&argv0).as_bytes());
            return EXIT_USAGE;
        }
        Some(b'm') => {
            let mut vm = request::new_vm(&cfg);
            let _ = vm.run_script(
                b"<?php echo \"[PHP Modules]\\n\"; foreach (get_loaded_extensions() as $e) echo \"$e\\n\"; echo \"\\n[Zend Modules]\\n\\n\";",
                "Standard input code",
            );
            vm.flush_stdout();
            return 0;
        }
        _ => {}
    }
    if a.info {
        let mut vm = request::new_vm(&cfg);
        let _ = vm.run_script(b"<?php phpinfo();", "Standard input code");
        vm.flush_stdout();
        return 0;
    }
    fpm_main(&cfg, &a)
}

static ERROR_LOG_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static STDERR_ORIGINAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static FORCE_STDERR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DAEMONIZE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `fpm_use_error_log()`: the error log is not used in the foreground on
/// a terminal (or with -O).
fn use_error_log() -> bool {
    use std::sync::atomic::Ordering;
    DAEMONIZE.load(Ordering::Relaxed)
        || (!os::isatty(libc::STDERR_FILENO) && !FORCE_STDERR.load(Ordering::Relaxed))
}

/// The descriptor `error_log` names, opened for appending.
fn open_log_file(el: &str) -> Option<i32> {
    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(el)
    {
        Ok(f) => Some(f.into_raw_fd()),
        Err(e) => {
            log::syserror(
                "fpm_stdio_open_error_log",
                0,
                &format!("failed to open error_log ({el})"),
                &e,
            );
            None
        }
    }
}

/// The original standard error (`fpm_stdio_save_original_stderr()`), and
/// then `fpm_stdio_open_error_log(0)`: called by the configuration's post
/// processing before the pools are checked, so their errors reach the
/// log.
pub fn open_error_log(g: &conf::Global) -> bool {
    use std::sync::atomic::Ordering;
    DAEMONIZE.store(g.daemonize, Ordering::Relaxed);
    if !g.daemonize {
        STDERR_ORIGINAL.store(os::dup(libc::STDERR_FILENO), Ordering::Relaxed);
    }
    let el = g.error_log.clone().unwrap_or_default();
    if el.eq_ignore_ascii_case("syslog") {
        os::openlog(
            g.syslog_ident.as_deref().unwrap_or("php-fpm"),
            g.syslog_facility,
        );
        ERROR_LOG_FD.store(log::SYSLOG, Ordering::Relaxed);
        if use_error_log() {
            log::set_fd(log::SYSLOG);
        }
        return true;
    }
    let Some(fd) = open_log_file(&el) else {
        return false;
    };
    ERROR_LOG_FD.store(fd, Ordering::Relaxed);
    if use_error_log() {
        log::set_fd_ex(fd, el == "/dev/stderr" || el == "/proc/self/fd/2");
    }
    true
}

/// Each pool's access log descriptor (`wp->log_fd`).
static ACCESS_FDS: std::sync::Mutex<Vec<Option<i32>>> = std::sync::Mutex::new(Vec::new());

/// Each pool's slowlog descriptor: opened by the master (the workers
/// write their own backtraces into it, whatever user they run as).
static SLOW_FDS: std::sync::Mutex<Vec<Option<i32>>> = std::sync::Mutex::new(Vec::new());

/// Opens each pool's `slowlog` for appending.
pub fn open_slowlogs(pools: &[conf::Pool]) -> bool {
    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let mut fds = Vec::new();
    for p in pools {
        let Some(path) = p
            .slowlog
            .as_deref()
            .filter(|s| !s.is_empty() && p.request_slowlog_timeout > 0)
        else {
            fds.push(None);
            continue;
        };
        match std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
        {
            Ok(f) => fds.push(Some(f.into_raw_fd())),
            Err(e) => {
                log::syserror(
                    "fpm_conf_process_all_pools",
                    0,
                    &format!("Unable to create or open slowlog({path})"),
                    &e,
                );
                return false;
            }
        }
    }
    if let Ok(mut s) = SLOW_FDS.lock() {
        *s = fds;
    }
    true
}

/// What a worker's slowlog handler needs about the running request.
#[derive(Clone)]
struct SlowCtx {
    sb: scoreboard::Scoreboard,
    slot: usize,
    fd: i32,
    pool: String,
    depth: i32,
    script: String,
}

thread_local! {
    static SLOW: std::cell::RefCell<Option<SlowCtx>> = const { std::cell::RefCell::new(None) };
}

/// The request VM's interrupt handler: a backtrace the master asked for
/// (the request is slower than `request_slowlog_timeout`) goes to the
/// slowlog in `fpm_php_trace_dump()`'s format. Written at the next safe
/// point (C reads the stopped worker's memory at once).
#[allow(clippy::unnecessary_wraps)] // an `InterruptFn`
fn slowlog_interrupt(vm: &mut php_vm::Vm) -> php_vm::ext::VmResult<()> {
    use php_runtime::{ArrayKey, Value};
    let Some(ctx) = SLOW.with(|s| s.borrow().clone()) else {
        return Ok(());
    };
    if !ctx.sb.take_trace(ctx.slot) {
        return Ok(());
    }
    let mut out = format!(
        "\n{}  [pool {}] pid {}\nscript_filename = {}\n",
        log::time_prefix(log::level() == log::Level::Debug),
        ctx.pool,
        os::getpid(),
        ctx.script
    );
    // DEBUG_BACKTRACE_IGNORE_ARGS: each frame's function and its call site.
    let bt = vm.call_function("debug_backtrace", vec![Value::Int(2), Value::Int(0)]);
    if let Ok(Value::Array(frames)) = bt {
        let mut left = ctx.depth;
        for (i, (_, frame)) in frames.iter().enumerate() {
            let Value::Array(f) = frame else { continue };
            let text = |k: &str| match f.get(&ArrayKey::from_bytes(k.as_bytes())) {
                Some(Value::Str(s)) => String::from_utf8_lossy(s.as_bytes()).into_owned(),
                Some(Value::Int(n)) => n.to_string(),
                _ => String::new(),
            };
            let mut name = text("function");
            if matches!(
                name.as_str(),
                "include" | "include_once" | "require" | "require_once"
            ) {
                name = "[INCLUDE_OR_EVAL]".into();
            }
            let _ = writeln!(
                out,
                "[0x{:016x}] {name}() {}:{}",
                i + 1,
                text("file"),
                text("line")
            );
            left -= 1;
            if left == 0 {
                break;
            }
        }
    }
    os::write_all(ctx.fd, out.as_bytes());
    Ok(())
}

/// `fpm_log_open(0)`, from the configuration's post processing.
pub fn open_access_logs(pools: &[conf::Pool]) -> bool {
    match access_log::open(pools) {
        Some(fds) => {
            if let Ok(mut a) = ACCESS_FDS.lock() {
                *a = fds;
            }
            true
        }
        None => false,
    }
}

/// `fpm_log_open(1)` (SIGUSR1): the number of pools whose access log was
/// reopened, or `None` when one failed.
pub fn reopen_access_logs(pools: &[conf::Pool]) -> Option<usize> {
    let fds = ACCESS_FDS.lock().map(|a| a.clone()).unwrap_or_default();
    match access_log::reopen(pools, &fds) {
        -1 => None,
        _ => Some(fds.iter().flatten().count()),
    }
}

/// `fpm_stdio_redirect_stderr_to_error_log()`.
fn redirect_stderr_to_error_log() {
    let fd = ERROR_LOG_FD.load(std::sync::atomic::Ordering::Relaxed);
    if use_error_log() && fd > 0 && fd != libc::STDERR_FILENO {
        os::dup2(fd, libc::STDERR_FILENO);
    }
}

/// `fpm_stdio_restore_original_stderr()`.
fn restore_original_stderr(close_after: bool) {
    let fd = STDERR_ORIGINAL.load(std::sync::atomic::Ordering::Relaxed);
    if fd != -1 {
        os::dup2(fd, libc::STDERR_FILENO);
        if close_after {
            os::close(fd);
        }
    }
}

/// SIGUSR1 (`fpm_got_signal()`): the error log is opened again (on the
/// same descriptor) and standard error tied to it once more.
pub fn reopen_error_log(conf: &conf::Conf) -> bool {
    use std::sync::atomic::Ordering;
    restore_original_stderr(false);
    let g = &conf.global;
    let el = g.error_log.clone().unwrap_or_default();
    let ok = if el.eq_ignore_ascii_case("syslog") {
        os::openlog(
            g.syslog_ident.as_deref().unwrap_or("php-fpm"),
            g.syslog_facility,
        );
        true
    } else if let Some(fd) = open_log_file(&el) {
        let old = ERROR_LOG_FD.load(Ordering::Relaxed);
        if old >= 0 {
            os::dup2(fd, old);
            os::close(fd);
        } else {
            ERROR_LOG_FD.store(fd, Ordering::Relaxed);
        }
        true
    } else {
        false
    };
    if ok {
        log::notice("fpm_got_signal", 0, "error log file re-opened");
    } else {
        log::error("fpm_got_signal", 0, "unable to re-opened error log file");
    }
    ok
}

/// SIGUSR1, after the access logs (`fpm_got_signal()`).
pub fn reopen_done() {
    redirect_stderr_to_error_log();
}

/// What `fpm_unix_conf_wp()` resolves for a pool's workers.
#[derive(Debug, Clone, Default)]
pub struct Ident {
    /// `wp->set_uid`, `wp->set_gid` (0: unchanged).
    pub uid: u32,
    pub gid: u32,
    /// `wp->set_user` (the name of a numeric `user`).
    pub set_user: Option<String>,
    /// `wp->user`, `wp->home`: the workers' `USER` and `HOME`.
    pub user: Option<String>,
    pub home: Option<String>,
}

/// `fpm_unix_is_id()`.
fn is_id(name: &str) -> bool {
    name.bytes().all(|b| b.is_ascii_digit())
}

/// `fpm_unix_parse_uid()` / `fpm_unix_parse_gid()`: a numeric ID below
/// `(uid_t) -1`.
fn parse_id(pool: &str, name: &str, what: &str) -> Option<u32> {
    match name.parse::<u64>() {
        Ok(v) if v < u64::from(u32::MAX) => u32::try_from(v).ok(),
        _ => {
            log::error(
                "fpm_unix_parse_uid",
                0,
                &format!("[pool {pool}] {what} ID '{name}' is out of range"),
            );
            None
        }
    }
}

/// `zlog(flags, ...)` with `ZLOG_ERROR` or `ZLOG_SYSERROR`.
fn log_flags(syserror: bool, function: &str, msg: &str) {
    if syserror {
        let e = std::io::Error::last_os_error();
        log::syserror(function, 0, msg, &e);
    } else {
        log::error(function, 0, msg);
    }
}

/// `fpm_unix_check_passwd()`.
fn check_passwd(pool: &str, name: Option<&str>, syserror: bool) -> bool {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return true;
    };
    if is_id(name) {
        return parse_id(pool, name, "user").is_some();
    }
    if os::getpwnam(name).is_none() {
        log_flags(
            syserror,
            "fpm_unix_get_passwd",
            &format!("[pool {pool}] cannot get uid for user '{name}'"),
        );
        return false;
    }
    true
}

/// `fpm_unix_check_group()`.
fn check_group(pool: &str, name: Option<&str>, syserror: bool) -> bool {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return true;
    };
    if is_id(name) {
        return parse_id(pool, name, "group").is_some();
    }
    if os::getgrnam(name).is_none() {
        log_flags(
            syserror,
            "fpm_unix_get_group",
            &format!("[pool {pool}] cannot get gid for group '{name}'"),
        );
        return false;
    }
    true
}

/// `fpm_unix_test_config()`: what `-t` checks of a pool.
fn unix_test_config(p: &conf::Pool) -> bool {
    let address_ok = || {
        let Some(addr) = p
            .listen_address
            .as_deref()
            .filter(|a| sockets::is_unix_address(a))
        else {
            return true;
        };
        let max = os::sun_path_len();
        if addr.len() < max {
            return true;
        }
        log_flags(
            true,
            "fpm_unix_check_listen_address",
            &format!(
                "[pool {}] cannot bind to UNIX socket '{addr}' as path is too long (found length: {}, maximal length: {max})",
                p.name,
                addr.len()
            ),
        );
        false
    };
    check_passwd(&p.name, p.user.as_deref(), false)
        && check_group(&p.name, p.group.as_deref(), false)
        && address_ok()
        && check_passwd(&p.name, p.listen_owner.as_deref(), true)
        && check_group(&p.name, p.listen_group.as_deref(), true)
}

/// `fpm_unix_conf_wp()`.
fn unix_conf_wp(p: &conf::Pool, run_as_root: bool) -> Option<Ident> {
    let mut id = Ident::default();
    let name = &p.name;
    if os::geteuid() == 0 {
        if let Some(u) = p.user.as_deref().filter(|u| !u.is_empty()) {
            if is_id(u) {
                id.uid = parse_id(name, u, "user")?;
                if let Some(user) = os::user_name(id.uid) {
                    if let Some((_, gid, _)) = os::getpwnam(&user) {
                        id.gid = gid;
                    }
                    id.set_user = Some(user);
                }
            } else {
                let Some((uid, gid, home)) = os::getpwnam(u) else {
                    log::error(
                        "fpm_unix_get_passwd",
                        0,
                        &format!("[pool {name}] cannot get uid for user '{u}'"),
                    );
                    return None;
                };
                id.uid = uid;
                id.gid = gid;
                id.user = Some(u.to_string());
                id.home = Some(home);
            }
        }
        if let Some(g) = p.group.as_deref().filter(|g| !g.is_empty()) {
            if is_id(g) {
                id.gid = parse_id(name, g, "group")?;
            } else {
                let Some(gid) = os::getgrnam(g) else {
                    log::error(
                        "fpm_unix_get_group",
                        0,
                        &format!("[pool {name}] cannot get gid for group '{g}'"),
                    );
                    return None;
                };
                id.gid = gid;
            }
        }
        if !run_as_root && (id.uid == 0 || id.gid == 0) {
            log::error(
                "fpm_unix_conf_wp",
                0,
                &format!("[pool {name}] please specify user and group other than root"),
            );
            return None;
        }
    } else {
        for (set, what) in [
            (p.user.as_deref().is_some_and(|s| !s.is_empty()), "user"),
            (p.group.as_deref().is_some_and(|s| !s.is_empty()), "group"),
            (p.chroot.as_deref().is_some_and(|s| !s.is_empty()), "chroot"),
            (p.process_priority != 64, "process.priority"),
        ] {
            if set {
                log::notice(
                    "fpm_unix_conf_wp",
                    0,
                    &format!("[pool {name}] '{what}' directive is ignored when FPM is not running as root"),
                );
            }
        }
        // "set up HOME and USER anyway".
        // SAFETY: `getuid` has no preconditions.
        let uid = unsafe { libc::getuid() };
        if let Some(user) = os::user_name(uid) {
            id.home = os::getpwnam(&user).map(|(_, _, h)| h);
            id.user = Some(user);
        }
    }
    Some(id)
}

/// `fpm_unix_init_main()` before the daemonizing: the master's limits.
fn unix_limits(g: &conf::Global) -> bool {
    if g.rlimit_files > 0 {
        if let Err(e) = os::set_rlimit(true, i64::from(g.rlimit_files)) {
            log::syserror("fpm_unix_init_main", 0, &format!("failed to set rlimit_files for this pool. Please check your system limits or decrease rlimit_files. setrlimit(RLIMIT_NOFILE, {})", g.rlimit_files), &e);
            return false;
        }
    }
    if g.rlimit_core != 0 {
        if let Err(e) = os::set_rlimit(false, i64::from(g.rlimit_core)) {
            log::syserror("fpm_unix_init_main", 0, &format!("failed to set rlimit_core for this pool. Please check your system limits or decrease rlimit_core. setrlimit(RLIMIT_CORE, {})", g.rlimit_core), &e);
            return false;
        }
    }
    true
}

/// `fpm_unix_init_main()`'s daemonizing: the calling process waits (10
/// seconds at most) for the master to report its initialization through
/// a pipe, then exits; `Some(fd)` (the pipe's write end) in the master.
fn daemonize() -> Result<Option<i32>, i32> {
    let (r, w) = match os::pipe() {
        Ok(p) => p,
        Err(e) => {
            log::syserror("fpm_unix_init_main", 0, "failed to create pipe", &e);
            return Err(master::EXIT_SOFTWARE);
        }
    };
    match os::fork() {
        Ok(os::Fork::Child) => {
            os::close(r);
            os::setsid();
            Ok(Some(w))
        }
        Ok(os::Fork::Parent(_)) => {
            os::close(w);
            let ready = os::poll(&[(r, libc::POLLIN)], 10_000);
            if ready.is_none_or(|v| v[0] == 0) {
                log::error(
                    "fpm_unix_init_main",
                    0,
                    "the master process didn't send back its status (via the pipe to the calling process)",
                );
                return Err(master::EXIT_SOFTWARE);
            }
            let mut buf = [0u8; 4];
            match os::read_status(r, &mut buf) {
                Ok(0) => {
                    log::error("fpm_unix_init_main", 0, "no data have been read from pipe");
                    Err(master::EXIT_SOFTWARE)
                }
                Ok(_) if i32::from_ne_bytes(buf) == 1 => Err(master::EXIT_OK),
                Ok(_) => Err(master::EXIT_SOFTWARE),
                Err(e) => {
                    log::syserror(
                        "fpm_unix_init_main",
                        0,
                        "failed to read from pipe",
                        &std::io::Error::from_raw_os_error(e),
                    );
                    Err(master::EXIT_SOFTWARE)
                }
            }
        }
        Err(e) => {
            log::syserror("fpm_unix_init_main", 0, "failed to daemonize", &e);
            Err(master::EXIT_SOFTWARE)
        }
    }
}

/// Tells the calling process how the master's initialization went.
fn report_to_caller(pipe: Option<i32>, ok: bool) {
    if let Some(w) = pipe {
        os::write_all(w, &i32::from(ok).to_ne_bytes());
        os::close(w);
    }
}

/// `fpm_init()` and `fpm_run()`.
#[allow(clippy::too_many_lines)]
fn fpm_main(cfg: &Config, a: &Args) -> i32 {
    use std::sync::atomic::Ordering;
    FORCE_STDERR.store(a.force_stderr, Ordering::Relaxed);
    // `fpm_stdio_init_main()`: standard input and output are /dev/null.
    if let Ok(null) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        use std::os::fd::AsRawFd;
        os::dup2(null.as_raw_fd(), libc::STDIN_FILENO);
        os::dup2(null.as_raw_fd(), libc::STDOUT_FILENO);
    }
    let fail = |pipe: Option<i32>| {
        log::error("fpm_init", 0, "FPM initialization failed");
        report_to_caller(pipe, false);
        master::EXIT_CONFIG
    };
    let Some(conf) = conf::load(
        a.fpm_config.as_deref().or(cfg.get("fpm.config")),
        a.prefix.as_deref(),
        a.pid.as_deref(),
        a.force_daemon,
        a.run_as_root,
    ) else {
        return fail(None);
    };
    if a.test > 0 {
        if !conf.pools.iter().all(unix_test_config) {
            return fail(None);
        }
        if a.test > 1 {
            conf::dump(&conf);
        }
        log::notice(
            "fpm_conf_init_main",
            0,
            &format!("configuration file {} test is successful", conf.file),
        );
        return master::EXIT_OK;
    }
    // `fpm_unix_init_main()`.
    if !unix_limits(&conf.global) {
        return fail(None);
    }
    let pipe = if conf.global.daemonize {
        match daemonize() {
            Ok(p) => p,
            Err(status) => return status,
        }
    } else {
        None
    };
    if conf.global.process_priority != 64 {
        if os::geteuid() == 0 {
            if let Err(e) = os::set_priority(conf.global.process_priority) {
                log::syserror(
                    "fpm_unix_init_main",
                    0,
                    "Unable to set priority for the master process",
                    &e,
                );
                return fail(pipe);
            }
        } else {
            log::notice(
                "fpm_unix_init_main",
                0,
                "'process.priority' directive is ignored when FPM is not running as root",
            );
        }
    }
    let mut idents = Vec::new();
    for p in &conf.pools {
        match unix_conf_wp(p, a.run_as_root) {
            Some(id) => idents.push(id),
            None => return fail(pipe),
        }
    }
    let signals = match os::init_master_signals() {
        Ok(fd) => fd,
        Err(e) => {
            log::syserror(
                "fpm_signals_init_main",
                0,
                "failed to init signals: socketpair()",
                &e,
            );
            return fail(pipe);
        }
    };
    sockets::import_inherited();
    let mut pools = Vec::new();
    for p in &conf.pools {
        let Some(l) = sockets::listen(p) else {
            return fail(pipe);
        };
        let Some(sb) =
            scoreboard::Scoreboard::new(usize::try_from(p.pm_max_children).unwrap_or(1).max(1))
        else {
            log::error(
                "fpm_scoreboard_init_main",
                0,
                "[pool] failed to allocate shared memory",
            );
            return fail(pipe);
        };
        pools.push(master::PoolRt::new(p.clone(), l, sb));
    }
    sockets::close_unused();
    if let Some(pid_file) = &conf.global.pid_file {
        let _ = std::fs::remove_file(pid_file);
        if let Err(e) = std::fs::write(pid_file, os::getpid().to_string()) {
            log::syserror(
                "fpm_conf_write_pid",
                0,
                &format!("Unable to create the PID file ({pid_file})."),
                &e,
            );
            return fail(pipe);
        }
    }
    // `fpm_stdio_init_final()`: standard error is the error log from now.
    redirect_stderr_to_error_log();
    if ERROR_LOG_FD.load(Ordering::Relaxed) == log::SYSLOG {
        os::dup2(libc::STDOUT_FILENO, libc::STDERR_FILENO);
    }
    log::set_launched();
    log::notice(
        "fpm_init",
        0,
        &format!("fpm is running, pid {}", os::getpid()),
    );
    report_to_caller(pipe, true);
    let mut m = master::Master::new(conf, pools, signals);
    match m.run() {
        master::Role::Exit(status) => {
            // `fpm_pctl_exit()`: `fpm_conf_unlink_pid()`, then the cleanups.
            if let Some(p) = &m.conf.global.pid_file {
                if let Err(e) = std::fs::remove_file(p) {
                    log::syserror(
                        "fpm_conf_unlink_pid",
                        0,
                        &format!("Unable to remove the PID file ({p})."),
                        &e,
                    );
                }
            }
            sockets::cleanup_exit();
            status
        }
        master::Role::Reload => {
            sockets::export_for_exec();
            restore_original_stderr(true);
            reexec()
        }
        master::Role::Child { pool, slot } => {
            let id = idents.get(pool).cloned().unwrap_or_default();
            worker(cfg, &m, pool, slot, &id)
        }
    }
}

/// `fpm_pctl_exec()`: the master runs again with its arguments.
fn reexec() -> i32 {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let shown: Vec<String> = args
        .iter()
        .map(|a| format!("\"{}\"", a.to_string_lossy()))
        .collect();
    log::notice(
        "fpm_pctl_exec",
        0,
        &format!(
            "reloading: execvp(\"{}\", {{{}}})",
            args.first()
                .map(|a| a.to_string_lossy())
                .unwrap_or_default(),
            shown.join(", ")
        ),
    );
    // "Blocking some signals before reexec" (the new image keeps the mask
    // until its handlers are installed; `Command::exec` would reset it).
    os::block_signals(&os::BLOCK_SIGNALS);
    let exe = std::env::current_exe()
        .map_or_else(|_| args[0].clone(), std::path::PathBuf::into_os_string);
    let e = os::execv(&exe, &args);
    log::syserror("fpm_pctl_exec", 0, "failed to reload: execvp() failed", &e);
    master::EXIT_SOFTWARE
}

/// `fpm_child_init()` and the request loop of `fpm_main.c`.
#[allow(clippy::too_many_lines)]
fn worker(cfg: &Config, m: &master::Master, pi: usize, slot: usize, id: &Ident) -> i32 {
    let pool = &m.pools[pi].conf;
    let sb = m.pools[pi].scoreboard;
    // `fpm_stdio_init_child()`: the error log is the master's business.
    let efd = ERROR_LOG_FD.load(std::sync::atomic::Ordering::Relaxed);
    if efd > 2 {
        os::close(efd);
    }
    log::set_fd(-1);
    // `fpm_unix_init_child()`.
    if pool.rlimit_files > 0 {
        if let Err(e) = os::set_rlimit(true, i64::from(pool.rlimit_files)) {
            log::syserror("fpm_unix_init_child", 0, &format!("[pool {}] failed to set rlimit_files for this pool. Please check your system limits or decrease rlimit_files. setrlimit(RLIMIT_NOFILE, {})", pool.name, pool.rlimit_files), &e);
        }
    }
    if pool.rlimit_core != 0 {
        if let Err(e) = os::set_rlimit(false, i64::from(pool.rlimit_core)) {
            log::syserror("fpm_unix_init_child", 0, &format!("[pool {}] failed to set rlimit_core for this pool. Please check your system limits or decrease rlimit_core. setrlimit(RLIMIT_CORE, {})", pool.name, pool.rlimit_core), &e);
        }
    }
    let is_root = os::geteuid() == 0;
    let mut made_chroot = false;
    if let Some(c) = pool.chroot.as_deref().filter(|c| is_root && !c.is_empty()) {
        if let Err(e) = os::chroot(c) {
            log::syserror(
                "fpm_unix_init_child",
                0,
                &format!("[pool {}] failed to chroot({c})", pool.name),
                &e,
            );
            return fail_child(pool);
        }
        made_chroot = true;
    }
    if let Some(d) = pool.chdir.as_deref().filter(|d| !d.is_empty()) {
        if let Err(e) = std::env::set_current_dir(d) {
            log::syserror(
                "fpm_unix_init_child",
                0,
                &format!("[pool {}] failed to chdir({d})", pool.name),
                &e,
            );
            return fail_child(pool);
        }
    } else if made_chroot && std::env::set_current_dir("/").is_err() {
        log::warning(
            "fpm_unix_init_child",
            0,
            &format!("[pool {}] failed to chdir(/)", pool.name),
        );
    }
    if is_root {
        if pool.process_priority != 64 {
            if let Err(e) = os::set_priority(pool.process_priority) {
                log::syserror(
                    "fpm_unix_init_child",
                    0,
                    &format!(
                        "[pool {}] Unable to set priority for this new process",
                        pool.name
                    ),
                    &e,
                );
                return fail_child(pool);
            }
        }
        if id.uid != 0 || id.gid != 0 {
            let user = id.set_user.as_deref().or(pool.user.as_deref());
            if let Err(e) = os::drop_privileges(
                user,
                (id.uid != 0).then_some(id.uid),
                (id.gid != 0).then_some(id.gid),
            ) {
                log::syserror(
                    "fpm_unix_init_child",
                    0,
                    &format!("[pool {}] failed to setuid({})", pool.name, id.uid),
                    &e,
                );
                return fail_child(pool);
            }
        }
    }
    if pool.process_dumpable {
        os::set_dumpable();
    }
    // `fpm_signals_init_child()`.
    os::init_child_signals();
    // `fpm_env_init_child()`: env[] (a `$NAME` value is the master's
    // variable), after clear_env.
    let wanted: Vec<(String, String)> = pool
        .env
        .iter()
        .map(|(k, v)| {
            let v = match v.strip_prefix('$') {
                Some(name) => std::env::var(name).unwrap_or_default(),
                None => v.clone(),
            };
            (k.clone(), v)
        })
        .collect();
    if pool.clear_env {
        for (k, _) in std::env::vars_os() {
            // SAFETY: the worker is single-threaded here (the master's
            // other thread only waits in `join`, and is not in a fork child).
            unsafe { std::env::remove_var(k) };
        }
    }
    for (k, v) in &wanted {
        // SAFETY: as above.
        unsafe { std::env::set_var(k, v) };
    }
    for (k, v) in [("USER", &id.user), ("HOME", &id.home)] {
        if let Some(v) = v {
            // SAFETY: as above.
            unsafe { std::env::set_var(k, v) };
        }
    }
    // `fpm_php_init_child()`: the pool's INI values become the worker's
    // master values (`php_admin_value`s locked), then the allowed clients.
    let mut ini = WorkerIni::new(cfg);
    for (k, v) in &pool.php_values {
        if !ini.apply(k, v, false) {
            log::error(
                "fpm_php_apply_defines",
                0,
                &format!("Unable to set php_value '{k}'"),
            );
        }
    }
    for (k, v) in &pool.php_admin_values {
        if !ini.apply(k, v, true) {
            log::error(
                "fpm_php_apply_defines",
                0,
                &format!("Unable to set php_admin_value '{k}'"),
            );
        }
    }
    // `php_child_init()`: each extension's `child_init_func`.
    for e in extensions::all() {
        e.child_init();
    }
    let allowed = pool
        .listen_allowed_clients
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(fastcgi::allowed_clients);
    let listener = if m.pools[pi].listening.unix {
        use std::os::fd::FromRawFd;
        // SAFETY: the pool's listening descriptor, owned by this process
        // from here on.
        fastcgi::Listener::Unix(unsafe {
            std::os::unix::net::UnixListener::from_raw_fd(m.pools[pi].listening.fd)
        })
    } else {
        use std::os::fd::FromRawFd;
        // SAFETY: as above.
        fastcgi::Listener::Tcp(unsafe {
            std::net::TcpListener::from_raw_fd(m.pools[pi].listening.fd)
        })
    };
    let max_children = pool.pm_max_children.to_string();
    let server = fastcgi::Server::new(
        listener,
        allowed,
        fastcgi::MgmtVars::new(Some(&max_children)),
    );
    let logging = request::cfg_bool(&ini.cfg, "fastcgi.logging", true);
    // A `pm.status_listen` pool shows the status of the pool it serves
    // (`scoreboard->shared`).
    let (shown, shown_sb) = match pool.shared.and_then(|i| m.pools.get(i)) {
        Some(sh) => (&sh.conf, sh.scoreboard),
        None => (pool, sb),
    };
    let status_pool = status::Pool {
        name: &shown.name,
        pm: shown.pm,
        scoreboard: shown_sb,
        status_path: pool.pm_status_path.as_deref(),
        ping_path: pool.ping_path.as_deref(),
        ping_response: pool.ping_response.as_deref(),
    };
    php_cli_server::sapi::set_fpm_status_hook(fpm_get_status);
    // "onced forked tell zlog to also send messages through
    // sapi_cgi_log_fastcgi()".
    log::set_external_logger(Some(Box::new(log_fastcgi)));
    STATUS.with(|s| {
        *s.borrow_mut() = Some(StatusInfo {
            name: shown.name.clone(),
            pm: shown.pm,
            scoreboard: shown_sb,
        });
    });
    // `fpm_log_init_child()`: this pool's access log descriptor stays open.
    let access_fds = ACCESS_FDS.lock().map(|a| a.clone()).unwrap_or_default();
    let access_fd = access_fds.get(pi).copied().flatten();
    for fd in access_fds.iter().flatten() {
        if Some(*fd) != access_fd {
            os::close(*fd);
        }
    }
    let access_format = pool
        .access_log
        .as_deref()
        .filter(|a| !a.is_empty())
        .and(pool.access_format.clone());
    let access = std::cell::RefCell::new(log::Stream::access(access_fd.unwrap_or(-1)));
    let max_requests = u64::try_from(pool.pm_max_requests).unwrap_or(0);
    // The slowlog: this pool's descriptor, and a watcher that turns the
    // master's request for a backtrace into an interrupt the request's VM
    // takes at its next safe point.
    let slow_fds = SLOW_FDS.lock().map(|a| a.clone()).unwrap_or_default();
    let slow_fd = slow_fds.get(pi).copied().flatten();
    for fd in slow_fds.iter().flatten() {
        if Some(*fd) != slow_fd {
            os::close(*fd);
        }
    }
    if slow_fd.is_some() {
        let _ = std::thread::Builder::new()
            .name("fpm-slowlog".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if sb.trace_pending(slot) {
                    php_vm::interrupt::request_interrupt();
                }
            });
    }
    // "else wp->config->request_slowlog_trace_depth = 20".
    let depth = if pool.request_slowlog_trace_depth > 0 {
        pool.request_slowlog_trace_depth
    } else {
        20
    };
    let mut requests = 0u64;
    let mut conn: Option<Rc<fastcgi::Connection>> = None;
    loop {
        sb.accepting(slot);
        let Some(params) = server.accept_request(&mut conn) else {
            break;
        };
        let Some(c) = conn.clone() else {
            break;
        };
        sb.reading_headers(slot);
        // A request from the previous request's trace is stale.
        sb.take_trace(slot);
        let cpu_start = os::cpu_us();
        // `init_request_info()` sees the values before this request's
        // `PHP_VALUE` and `PHP_ADMIN_VALUE`, which then stay for the
        // worker's later requests too (they alter the master values).
        let settings = request::request_settings(&ini.cfg);
        if logging {
            REQUEST.with(|r| *r.borrow_mut() = Some((c.clone(), false)));
        }
        ini.request_defines(&params);
        let reply =
            |info: &env::RequestInfo, _env: &env::Env| -> Option<php_cli_server::sapi::CgiReply> {
                sb.info(
                    slot,
                    info.request_method.as_deref().unwrap_or_default(),
                    info.request_uri.as_deref().unwrap_or_default(),
                    info.query_string.as_deref().unwrap_or_default(),
                    info.path_translated.as_deref().unwrap_or_default(),
                    &auth_user(info.authorization.as_deref()),
                    u64::try_from(info.content_length).unwrap_or(0),
                );
                if let Some(fd) = slow_fd {
                    let script = info.path_translated.as_deref().unwrap_or_default();
                    SLOW.with(|s| {
                        *s.borrow_mut() = Some(SlowCtx {
                            sb,
                            slot,
                            fd,
                            pool: pool.name.clone(),
                            depth,
                            script: String::from_utf8_lossy(script).into_owned(),
                        });
                    });
                }
                // "it's certainly not an HTTP over fcgi request".
                info.request_method.as_ref()?;
                let head = info.request_method.as_deref() == Some(b"HEAD");
                if let Some(r) = status::handle(
                    &status_pool,
                    info.request_uri.as_deref().unwrap_or_default(),
                    info.query_string.as_deref().unwrap_or_default(),
                    head,
                ) {
                    sb.executing(slot);
                    return Some(r);
                }
                let Some(path) = &info.path_translated else {
                    log::debug("fpm_main", 0, "Primary script unknown");
                    return Some(php_cli_server::sapi::CgiReply {
                        status: 404,
                        headers: Vec::new(),
                        body: b"File not found.\n".to_vec(),
                    });
                };
                if limited(pool, path) {
                    return Some(php_cli_server::sapi::CgiReply {
                        status: 403,
                        headers: Vec::new(),
                        body: b"Access denied.\n".to_vec(),
                    });
                }
                None
            };
        let executing = |_: &env::RequestInfo, _: &env::Env| sb.executing(slot);
        // `fpm_request_end()` then `fpm_log_write(NULL)`, before the request
        // ends for the client.
        let done_hook = |done: &request::RequestDone| {
            sb.end(
                slot,
                u64::try_from(php_memlimit::peak()).unwrap_or(0),
                os::cpu_us().saturating_sub(cpu_start),
            );
            if let Some(format) = &access_format {
                let proc = sb.proc(slot);
                let client = server.last_client_ip();
                let entry = access_log::Entry {
                    pool: &pool.name,
                    proc: &proc,
                    env: &done.env,
                    headers: &done.headers,
                    status: done.response_code,
                    client_ip: client.as_deref(),
                    content_length: done.info.content_length,
                };
                access_log::write(
                    format,
                    &mut access.borrow_mut(),
                    Some(&entry),
                    &pool.access_suppress_paths,
                );
            }
        };
        let hooks = request::RequestHooks {
            extra_ini: Vec::new(),
            reply: Some(&reply),
            executing: Some(&executing),
            done: Some(&done_hook),
            request_info: Some(request_info::init_request_info),
            vm_setup: Some(|vm| vm.register_interrupt_handler(slowlog_interrupt)),
            open_failure_log: Some(|msg| log::error("fpm_main", 0, msg)),
            locked: ini.locked.clone(),
            log: Some(log_message),
        };
        let _ = request::fastcgi_request(&ini.cfg, &settings, logging, params, &c, &hooks);
        REQUEST.with(|r| *r.borrow_mut() = None);
        SLOW.with(|s| *s.borrow_mut() = None);
        // `fpm_stdio_flush_child()`: the master ends the line it relays.
        os::write_all(libc::STDERR_FILENO, b"\0fscf\0");
        requests += 1;
        // `EG_FLAGS_IN_SHUTDOWN` stays set until the next request starts.
        ini.probe.in_shutdown = true;
        if max_requests > 0 && requests == max_requests {
            c.finish_request(true);
            break;
        }
        if os::soft_quit() {
            break;
        }
    }
    master::EXIT_OK
}

thread_local! {
    /// The request being served while `fastcgi.logging` is on, and whether
    /// a message has been sent for it (`fcgi_logging_request_started`).
    static REQUEST: std::cell::RefCell<Option<(Rc<fastcgi::Connection>, bool)>> =
        const { std::cell::RefCell::new(None) };
}

/// `sapi_cgi_log_fastcgi()`: a worker's log message to the web server as
/// `FCGI_STDERR`, `; ` between the messages of one request.
fn log_fastcgi(_level: log::Level, msg: &[u8]) {
    if msg.is_empty() {
        return;
    }
    REQUEST.with(|r| {
        if let Some((c, started)) = r.borrow_mut().as_mut() {
            use php_cli_server::sapi::FastCgiStreams;
            if *started {
                c.stderr(b"; ");
            } else {
                *started = true;
            }
            c.stderr(msg);
        }
    });
}

/// `sapi_cgi_log_message()`: `zlog_msg(ZLOG_NOTICE, "PHP message: ", message)`.
fn log_message(msg: &[u8]) {
    log::msg(
        log::Level::Notice,
        "sapi_cgi_log_message",
        621,
        b"PHP message: ",
        msg,
    );
}

/// `SG(request_info).auth_user` (`php_handle_auth_data()`): the user of
/// `Authorization: Basic ...`.
fn auth_user(auth: Option<&[u8]>) -> Vec<u8> {
    let Some(b64) = auth.and_then(|a| a.strip_prefix(b"Basic ")) else {
        return Vec::new();
    };
    php_stdstr::base64::base64_decode(b64, false)
        .and_then(|d| d.iter().position(|&c| c == b':').map(|i| d[..i].to_vec()))
        .unwrap_or_default()
}

/// `fpm_child_init()` failed.
fn fail_child(pool: &conf::Pool) -> i32 {
    log::error(
        "fpm_child_init",
        0,
        &format!("[pool {}] child failed to initialize", pool.name),
    );
    master::EXIT_SOFTWARE
}

/// A worker's INI master values: the configuration with the pool's
/// `php_value`s and `php_admin_value`s and the requests' `PHP_VALUE`s and
/// `PHP_ADMIN_VALUE`s (`fpm_php_zend_ini_alter_master()`).
struct WorkerIni {
    cfg: Config,
    /// Entries set in `ZEND_INI_SYSTEM` mode.
    locked: Vec<String>,
    /// The directives that exist (`EG(ini_directives)`).
    known: std::collections::HashSet<String>,
    /// A VM whose directives' on-modify handlers judge each value.
    probe: php_vm::Vm,
}

impl WorkerIni {
    fn new(cfg: &Config) -> WorkerIni {
        let mut probe = request::new_vm(cfg);
        // The values are applied between requests, with the executor
        // inactive: a handler's diagnostic goes to the log as
        // `sapi_cgi_log_message()` sends it (and on to the web server).
        probe.executor_active = false;
        probe.log_hook = Some(Box::new(log_message));
        let known = probe
            .ini
            .ini_get_all(None)
            .map(|all| all.into_iter().map(|e| e.name).collect())
            .unwrap_or_default();
        WorkerIni {
            cfg: cfg.clone(),
            locked: Vec::new(),
            known,
            probe,
        }
    }

    /// `fpm_php_apply_defines_ex()`: `false` for a directive that does not
    /// exist (`extension` loads nothing: every extension is built in).
    fn apply(&mut self, name: &str, value: &str, admin: bool) -> bool {
        if name == "extension" {
            return true;
        }
        if !self.known.contains(name) {
            return false;
        }
        // "if (!ini_entry->on_modify || ini_entry->on_modify(...) ==
        // SUCCESS)": a value its handler rejects is not set (and that is
        // not a failure).
        if let Some(Ok(false) | Err(_)) =
            self.probe
                .run_ini_modify_hook(name, value.as_bytes(), php_vm::ext::IniStage::Activate)
        {
            return true;
        }
        self.cfg.entries.push((name.to_string(), value.to_string()));
        if admin && !self.locked.iter().any(|n| n == name) {
            self.locked.push(name.to_string());
        }
        true
    }

    /// `PHP_VALUE` (user mode) and `PHP_ADMIN_VALUE` (system mode) of a
    /// request, parsed as INI (`fastcgi_ini_parser()`).
    fn request_defines(&mut self, params: &fastcgi::Env) {
        for (key, admin) in [
            (b"PHP_VALUE".as_slice(), false),
            (b"PHP_ADMIN_VALUE".as_slice(), true),
        ] {
            let Some((_, v)) = params.iter().find(|(k, _)| k == key) else {
                continue;
            };
            let mut text = v.clone();
            text.push(b'\n');
            let mut entries = FcgiIni(Vec::new());
            let _ = php_support::ini::parse_ini(
                &text,
                "Unknown",
                php_support::ini::ScannerMode::Normal,
                &mut conf::Hooks,
                &mut entries,
            );
            for e in entries.0 {
                match e {
                    Ok((k, v)) => {
                        if !self.apply(&k, &v, admin) {
                            log::error(
                                "fastcgi_ini_parser",
                                0,
                                &format!(
                                    "Passing INI directive through FastCGI: unable to set '{k}'"
                                ),
                            );
                        }
                    }
                    Err(msg) => log::error("fastcgi_ini_parser", 0, &msg),
                }
            }
        }
    }
}

/// The entries of a `PHP_VALUE`, or the error `fastcgi_ini_parser()` logs.
struct FcgiIni(Vec<Result<(String, String), String>>);

impl php_support::ini::IniParserCallback for FcgiIni {
    fn section(&mut self, _name: &[u8]) {
        self.0.push(Err(
            "Passing INI directive through FastCGI: only classic entries are allowed".into(),
        ));
    }

    fn entry(&mut self, key: &[u8], value: Option<php_support::ini::IniScalar>) {
        let key = String::from_utf8_lossy(key).into_owned();
        if key.is_empty() {
            self.0.push(Err(
                "Passing INI directive through FastCGI: empty key".into()
            ));
            return;
        }
        match value {
            Some(v) => self.0.push(Ok((key, conf::scalar(v)))),
            None => self.0.push(Err(format!(
                "Passing INI directive through FastCGI: empty value for key '{key}'"
            ))),
        }
    }

    fn pop_entry(&mut self, _key: &[u8], _value: php_support::ini::IniScalar, _offset: &[u8]) {
        self.0.push(Err(
            "Passing INI directive through FastCGI: only classic entries are allowed".into(),
        ));
    }
}

/// `fpm_php_limit_extensions()`: whether `path` is refused by
/// `security.limit_extensions`.
fn limited(pool: &conf::Pool, path: &[u8]) -> bool {
    if pool.limit_extensions.is_empty() {
        return false;
    }
    if pool
        .limit_extensions
        .iter()
        .any(|e| path.ends_with(e.as_bytes()))
    {
        return false;
    }
    log::notice(
        "fpm_php_limit_extensions",
        0,
        &format!(
            "Access to the script '{}' has been denied (see security.limit_extensions)",
            String::from_utf8_lossy(path)
        ),
    );
    true
}

/// What `fpm_get_status()` reads in a worker.
struct StatusInfo {
    name: String,
    pm: conf::Pm,
    scoreboard: scoreboard::Scoreboard,
}

thread_local! {
    static STATUS: std::cell::RefCell<Option<StatusInfo>> = const { std::cell::RefCell::new(None) };
}

/// `fpm_get_status()` (`fpm_status_export_to_zval()`): the pool's status
/// as an array.
#[allow(clippy::unnecessary_wraps)] // a builtin
fn fpm_get_status(
    _vm: &mut php_vm::Vm,
    _a: &mut php_vm::ext::Args,
) -> php_vm::ext::VmResult<php_runtime::Value> {
    use php_runtime::{ArrayKey, HashTable, Value, ZArray};
    let Some((name, pm, sb)) = STATUS.with(|s| {
        s.borrow()
            .as_ref()
            .map(|s| (s.name.clone(), s.pm, s.scoreboard))
    }) else {
        return Ok(Value::Bool(false));
    };
    let t = sb.totals();
    let (idle, active) = sb.count();
    let (now, now_usec) = os::now();
    let now_us = u64::try_from(now).unwrap_or(0) * 1_000_000 + u64::from(now_usec);
    let mut h = HashTable::new();
    let put = |h: &mut HashTable, k: &str, v: Value| {
        h.insert(ArrayKey::Str(k.into()), v);
    };
    let int = |n: u64| Value::Int(i64::try_from(n).unwrap_or(i64::MAX));
    let text = |s: &str| Value::Str(s.as_bytes().into());
    let dash = |s: &str| {
        if s.is_empty() {
            "-".to_string()
        } else {
            s.to_string()
        }
    };
    put(&mut h, "pool", text(&name));
    put(&mut h, "process-manager", text(pm.name()));
    put(&mut h, "start-time", Value::Int(t.start_epoch));
    put(&mut h, "start-since", Value::Int(now - t.start_epoch));
    put(&mut h, "accepted-conn", int(t.requests));
    put(&mut h, "listen-queue", int(t.lq));
    put(&mut h, "max-listen-queue", int(t.lq_max));
    put(&mut h, "listen-queue-len", int(t.lq_len));
    put(&mut h, "idle-processes", int(idle));
    put(&mut h, "active-processes", int(active));
    put(&mut h, "total-processes", int(idle + active));
    put(
        &mut h,
        "max-active-processes",
        int(t.active_max.max(active)),
    );
    put(&mut h, "max-children-reached", int(t.max_children_reached));
    put(&mut h, "slow-requests", int(t.slow));
    put(&mut h, "memory-peak", int(t.memory_peak));
    let mut procs = Vec::new();
    for p in sb.procs() {
        let idle_slot = p.stage == Some(scoreboard::Stage::Accepting);
        let mut ph = HashTable::new();
        put(&mut ph, "pid", Value::Int(i64::from(p.pid)));
        put(
            &mut ph,
            "state",
            text(p.stage.map_or("", scoreboard::Stage::name)),
        );
        put(&mut ph, "start-time", Value::Int(p.start_epoch));
        put(&mut ph, "start-since", Value::Int(now - p.start_epoch));
        put(&mut ph, "requests", int(p.requests));
        let duration = if idle_slot {
            p.duration_us
        } else {
            now_us.saturating_sub(p.accepted_us)
        };
        put(&mut ph, "request-duration", int(duration));
        put(&mut ph, "request-method", text(&dash(&p.method)));
        put(&mut ph, "request-uri", text(&p.uri));
        put(&mut ph, "query-string", text(&p.query));
        put(&mut ph, "request-length", int(p.content_length));
        put(&mut ph, "user", text(&dash(&p.user)));
        put(&mut ph, "script", text(&dash(&p.script)));
        #[allow(clippy::cast_precision_loss)]
        let cpu = if idle_slot && p.duration_us > 0 {
            p.cpu_us as f64 / p.duration_us as f64 * 100.0
        } else {
            0.0
        };
        put(&mut ph, "last-request-cpu", Value::Float(cpu));
        put(
            &mut ph,
            "last-request-memory",
            int(if idle_slot { p.memory } else { 0 }),
        );
        procs.push(Value::Array(ZArray::from_table(ph)));
    }
    put(&mut h, "procs", Value::Array(ZArray::from_values(procs)));
    Ok(Value::Array(ZArray::from_table(h)))
}

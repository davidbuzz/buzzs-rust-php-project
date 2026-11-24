// Derived from php-src sapi/cgi/cgi_main.c. Similarity metric to original: 14%
//   Copyright © The PHP Group and Contributors.
//   Authors: Rasmus Lerdorf <rasmus@lerdorf.on.ca>
//            Stig Bakken <ssb@php.net>
//            Zeev Suraski <zeev@php.net>
//            FastCGI: Ben Mansell <php@slimyhorror.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `php-cgi` binary: the CGI/1.1 and `FastCGI` SAPI (ref:
//! `sapi/cgi/cgi_main.c`, `main/fastcgi.c`).
//!
//! `FastCGI`: `-b <address:port>|<port>|<path>` listens on a TCP or Unix
//! socket, and a listening socket passed as standard input (`mod_fcgid`,
//! spawn-fcgi) is used as it is. `PHP_FCGI_MAX_REQUESTS` (default 500),
//! `PHP_FCGI_CHILDREN` (pre-forked children), `FCGI_WEB_SERVER_ADDRS`
//! and `fastcgi.logging` behave as in php-src; `PHP_FCGI_BACKLOG` is
//! checked, but the listen backlog is the standard library's.
//!
//! It shares the configuration search, the extension list and the startup
//! checks with the `php` binary, and runs the request through the
//! built-in server's request machinery in CGI mode
//! ([`php_cli_server::sapi::CgiInfo`]): the environment becomes
//! `$_SERVER`, the body is read from standard input (`CONTENT_LENGTH`
//! bytes), and the response head (`Status:`, `X-Powered-By:`,
//! `Content-type:`) precedes the output unless `-q`.
//!
//! `run-tests.php` runs it as `php-cgi [-d ...] -C <file>` with
//! `REDIRECT_STATUS=1`, `REQUEST_METHOD`, `QUERY_STRING`,
//! `SCRIPT_FILENAME`/`PATH_TRANSLATED` (the test file), `HTTP_COOKIE`,
//! `CONTENT_TYPE` and `CONTENT_LENGTH` set and the body on standard input.

#[allow(dead_code)]
#[path = "../config.rs"]
mod config;
mod env;
#[allow(dead_code)]
#[path = "../extensions.rs"]
mod extensions;
mod fastcgi;
#[allow(dead_code)]
#[path = "../getopt.rs"]
mod getopt;
#[path = "../main_stack.rs"]
mod main_stack;
#[allow(dead_code)]
#[path = "../memcap.rs"]
mod memcap;
// `php-fpm` uses the parts `php-cgi` does not (`set_fpm`, `RequestDone`'s
// bookkeeping fields).
#[allow(dead_code)]
mod request;
#[path = "../startup.rs"]
mod startup;
mod sys;

use config::Config;
use getopt::{GetOpt, Opt, INVALID};
use php_cli_server::sapi::{CgiInfo, RequestCtx};
use php_runtime::ZStr;
use request::{
    activate_config, build_request, cfg_bool, fastcgi_request, new_prepared_vm, new_vm,
    request_settings, sapi_ini, VM_STACK,
};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::rc::Rc;

/// `PHP_CLI_BUILD_DATE` (shared with the `php` binary's build script).
const BUILD_DATE: &str = env!("PHP_CLI_BUILD_DATE");

/// The option table (`OPTIONS[]` of `cgi_main.c`).
const OPTIONS: &[Opt] = &[
    Opt {
        ch: b'a',
        need_param: 0,
        name: "interactive",
    },
    Opt {
        ch: b'b',
        need_param: 1,
        name: "bindpath",
    },
    Opt {
        ch: b'C',
        need_param: 0,
        name: "no-chdir",
    },
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
        ch: b'f',
        need_param: 1,
        name: "file",
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
        ch: b'l',
        need_param: 0,
        name: "syntax-check",
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
        ch: b'q',
        need_param: 0,
        name: "no-header",
    },
    Opt {
        ch: b's',
        need_param: 0,
        name: "syntax-highlight",
    },
    Opt {
        ch: b's',
        need_param: 0,
        name: "syntax-highlighting",
    },
    Opt {
        ch: b'w',
        need_param: 0,
        name: "strip",
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
        ch: b'T',
        need_param: 1,
        name: "timing",
    },
    Opt {
        ch: b'-',
        need_param: 0,
        name: "",
    },
];

const SECURITY_ALERT: &str = "<b>Security Alert!</b> The PHP CGI cannot be accessed directly.\n\n\
<p>This PHP CGI binary was compiled with force-cgi-redirect enabled.  This\n\
means that a page will only be served up if the REDIRECT_STATUS CGI variable is\n\
set, e.g. via an Apache Action directive.</p>\n\
<p>For more information as to <i>why</i> this behaviour exists, see the <a href=\"https://www.php.net/security.cgi-bin\">\
manual page for CGI security</a>.</p>\n\
<p>For more information about changing this behaviour or re-enabling this webserver,\n\
consult the installation file that came with this distribution, or visit \n\
<a href=\"https://www.php.net/install.windows\">the manual page</a>.</p>\n";

const MODULES_SNIPPET: &[u8] = b"<?php
$m = get_loaded_extensions();
usort($m, 'strcasecmp');
echo \"[PHP Modules]\\n\";
foreach ($m as $e) echo $e, \"\\n\";
echo \"\\n[Zend Modules]\\n\";
$z = get_loaded_extensions(true);
sort($z);
foreach ($z as $e) echo $e, \"\\n\";
echo \"\\n\";
";

/// `php_cgi_usage()`.
fn usage(argv0: &[u8]) -> Vec<u8> {
    let prog = argv0
        .iter()
        .rposition(|&c| c == b'/')
        .map_or(&b"php-cgi"[..], |i| &argv0[i + 1..]);
    let prog = String::from_utf8_lossy(prog);
    format!(
        "Usage: {prog} [-q] [-h] [-s] [-v] [-i] [-f <file>]\n       {prog} <file> [args...]\n  -a               Run interactively\n  -b <address:port>|<port> Bind Path for external FASTCGI Server mode\n  -C               Do not chdir to the script's directory\n  -c <path>|<file> Look for php.ini file in this directory\n  -n               No php.ini file will be used\n  -d foo[=bar]     Define INI entry foo with value 'bar'\n  -e               Generate extended information for debugger/profiler\n  -f <file>        Parse <file>.  Implies `-q'\n  -h               This help\n  -i               PHP information\n  -l               Syntax check only (lint)\n  -m               Show compiled in modules\n  -q               Quiet-mode.  Suppress HTTP Header output.\n  -s               Display colour syntax highlighted source.\n  -v               Version number\n  -w               Display source with stripped comments and whitespace.\n  -T <count>       Measure execution time of script repeated <count> times.\n"
    )
    .into_bytes()
}

/// `php_print_version()` for this SAPI.
fn version_banner() -> String {
    format!(
        "PHP {} (cgi-fcgi) (built: {BUILD_DATE}) (NTS)\nCopyright \u{a9} The PHP Group and Contributors\nZend Engine v{}, Copyright \u{a9} Zend by Perforce\n",
        php_vm::PHP_VERSION,
        php_vm::ZEND_VERSION
    )
}

/// The behaviour chosen on the command line (`PHP_MODE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Behavior {
    #[default]
    Standard,
    Highlight,
    Lint,
    Strip,
}

/// The options of the third `php_getopt()` pass (only when not started
/// by a web server).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CommandLine {
    behavior: Behavior,
    no_headers: bool,
    no_chdir: bool,
    script_file: Option<Vec<u8>>,
    /// `-i`, `-m`, `-v`.
    info: Option<u8>,
    /// `-a`: "Interactive mode enabled" is printed (nothing else changes).
    interactive: bool,
    /// `-T [<warmup>,]<count>`: the script runs `warmup` times untimed,
    /// then `count` times, and the time taken goes to standard error.
    timing: Option<(i64, i64)>,
    /// Index of the first non-option argument.
    optind: usize,
}

/// The third option pass of `main()` (`!cgi && !fastcgi`).
fn parse_command_line(argv: &[Vec<u8>]) -> CommandLine {
    let mut cl = CommandLine::default();
    let mut g = GetOpt::new(argv, OPTIONS, false);
    while let Some((c, v)) = g.next_opt() {
        match u8::try_from(c).unwrap_or(0) {
            b'a' => cl.interactive = true,
            b'C' => cl.no_chdir = true,
            b'f' => {
                cl.script_file = v;
                cl.no_headers = true;
            }
            b'i' | b'm' | b'v' => {
                cl.info = Some(u8::try_from(c).unwrap_or(0));
                break;
            }
            b'l' => {
                cl.no_headers = true;
                cl.behavior = Behavior::Lint;
            }
            b'q' => cl.no_headers = true,
            b's' => cl.behavior = Behavior::Highlight,
            b'w' => cl.behavior = Behavior::Strip,
            _ => {}
        }
    }
    cl.optind = g.optind;
    cl
}

php_memlimit::install_global_allocator!();

/// An allocation crossed the hard ceiling above `memory_limit`: PHP's
/// message on stderr, then exit 137 without unwinding (as the `php` binary).
fn hard_limit(e: php_memlimit::HardLimitExceeded) -> ! {
    let _ = std::io::stderr().write_all(format!("{e} in Unknown on line 0\n").as_bytes());
    // SAFETY: `_exit` terminates the process immediately; nothing here
    // relies on destructors or atexit handlers (PHP does the same).
    unsafe { libc::_exit(137) }
}

fn main() {
    let argv: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
    // php-src's huge pages: only with USE_ZEND_ALLOC_HUGE_PAGES set (first,
    // before the heap grows).
    php_memlimit::set_huge_pages(php_memlimit::huge_pages_from_env(
        std::env::var_os("USE_ZEND_ALLOC_HUGE_PAGES")
            .as_deref()
            .map(std::os::unix::ffi::OsStrExt::as_bytes),
    ));
    let on_main_thread = main_stack::ensure(&argv);
    // The request heap: `memory_limit` accounting and the address-space
    // cap, as in the `php` binary.
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
    // SAFETY: ignoring SIGPIPE like cgi_main.c, so writes to a closed pipe
    // fail with EPIPE instead of killing the process.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // The VM runs on the main thread, as in C: the process stays
    // single-threaded, so `PHP_FCGI_CHILDREN` children fork from a
    // single-threaded parent.
    if on_main_thread {
        finish(run(&argv));
    }
    // Fallback: a thread with a large stack (deep recursion); the signals
    // go to it (see `mask_process_signals`).
    sys::mask_process_signals(true);
    let status = std::thread::Builder::new()
        .stack_size(usize::try_from(VM_STACK).unwrap_or(usize::MAX))
        .spawn(move || {
            sys::mask_process_signals(false);
            run(&argv)
        })
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or(255);
    finish(status);
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

/// `main()` of `cgi_main.c`.
#[allow(clippy::too_many_lines)]
fn run(argv: &[Vec<u8>]) -> i32 {
    php_memlimit::bind_to_current_thread();
    let argv0 = argv.first().cloned().unwrap_or_else(|| b"php-cgi".to_vec());
    // `fcgi_is_fastcgi()`: a web server passed a listening socket as
    // standard input.
    let stdin_fastcgi = sys::stdin_is_listening_socket();
    let mut environment = env::Env::from_process();
    let cgi = !stdin_fastcgi && env::is_cgi(&environment);
    let skip_getopt = env::skip_getopt(environment.get("QUERY_STRING").as_deref());
    let mut bindpath: Option<String> = None;

    // First pass: the options needed before startup.
    let mut opts = config::Options {
        binary: std::env::current_exe().ok(),
        cgi: true,
        ..config::Options::default()
    };
    if !skip_getopt {
        let mut g = GetOpt::new(argv, OPTIONS, false);
        while let Some((c, v)) = g.next_opt() {
            match u8::try_from(c).unwrap_or(0) {
                b'c' => opts.path_override = v,
                b'n' => opts.ignore = true,
                b'd' => opts
                    .defines
                    .extend(php_vm::parse_cli_define(&v.unwrap_or_default())),
                // Started from a shell as an external FastCGI server.
                b'b' if !stdin_fastcgi => {
                    bindpath = v.map(|v| String::from_utf8_lossy(&v).into_owned());
                }
                _ => {}
            }
        }
    }
    let cfg = Config::load(&opts);
    for w in &cfg.warnings {
        let _ = std::io::stderr().write_all(format!("PHP:  {w}\n").as_bytes());
    }
    if let Err(status) = startup::module_startup(&cfg) {
        return status;
    }

    // `cgi.force_redirect`: refuse to run unless the web server says it
    // redirected the request.
    if cgi && cfg_bool(&cfg, "cgi.force_redirect", true) {
        let var = cfg
            .get("cgi.redirect_status_env")
            .filter(|v| !v.is_empty())
            .unwrap_or("REDIRECT_STATUS");
        if environment.get(var).is_none() {
            out(SECURITY_ALERT.as_bytes());
            return 255;
        }
    }

    // FastCGI: the `-b` socket, or the one on standard input.
    let listener = if let Some(path) = &bindpath {
        if let Ok(b) = std::env::var("PHP_FCGI_BACKLOG") {
            let backlog = i64::from(php_support::atoi(b.as_bytes()));
            if backlog < -1 || backlog > i64::from(libc::SOMAXCONN) {
                let _ = std::io::stderr().write_all(
                    format!(
                        "Invalid backlog {backlog}, needs to be between -1 and {}\n",
                        libc::SOMAXCONN
                    )
                    .as_bytes(),
                );
                return 255;
            }
        }
        let Some(l) = fastcgi::listen(path) else {
            let _ = std::io::stderr().write_all(
                format!("Couldn't create FastCGI listen socket on port {path}\n").as_bytes(),
            );
            return 255;
        };
        sys::setup_signals();
        Some(l)
    } else if stdin_fastcgi {
        sys::setup_signals();
        Some((sys::stdin_listener(), None))
    } else {
        None
    };
    if let Some((listener, allowed)) = listener {
        finish(fastcgi_main(
            &cfg,
            argv,
            &argv0,
            skip_getopt,
            listener,
            allowed,
        ));
    }

    // Second pass: help and invalid options, and `-T`.
    let timing = match second_pass(argv, &argv0, skip_getopt) {
        Ok(t) => t,
        Err(status) => return status,
    };

    let settings = request_settings(&cfg);
    let mut info = env::init_request_info(&mut environment, &settings, &env::RealFs);
    environment.apply();

    let mut cl = CommandLine {
        timing,
        ..CommandLine::default()
    };
    let mut source: Option<Vec<u8>> = None;
    // The private copy of standard input `stdin_to_temp` made, if any.
    let mut stdin_copy: Option<Vec<u8>> = None;
    if !cgi && !skip_getopt {
        cl = parse_command_line(argv);
        cl.timing = timing;
        if cl.interactive {
            out(b"Interactive mode enabled\n\n");
        }
        match cl.info {
            Some(b'v') => {
                out(version_banner().as_bytes());
                return 0;
            }
            Some(b'i') => source = Some(b"<?php phpinfo();".to_vec()),
            Some(b'm') => {
                source = Some(MODULES_SNIPPET.to_vec());
                cl.no_headers = true;
            }
            _ => {}
        }
        let mut optind = cl.optind;
        if let Some(f) = &cl.script_file {
            info.path_translated = Some(f.clone());
        } else if argv.len() > optind {
            info.path_translated = Some(argv[optind].clone());
            optind += 1;
        }
        // The remaining arguments form the query string.
        if info.query_string.is_none() && argv.len() > optind {
            let sep = cfg
                .get("arg_separator.input")
                .and_then(|s| s.as_bytes().first().copied())
                .unwrap_or(b'&');
            info.query_string = Some(argv[optind..].join(&sep));
        }
    }

    match cl.behavior {
        Behavior::Lint => {
            // `goto do_repeat`: the arguments after the first file are
            // linted in turn (`-f` names the first one).
            let mut files = vec![info.path_translated.clone()];
            if !cgi {
                let mut optind = cl.optind;
                while argv.len() > optind + 1 {
                    optind += 1;
                    files.push(Some(argv[optind].clone()));
                }
            }
            let mut status = 0;
            for f in files {
                match lint(&cfg, f.as_deref()) {
                    Some(0) => {}
                    Some(_) => status = 255,
                    None => return 255,
                }
            }
            return status;
        }
        Behavior::Highlight | Behavior::Strip
            if source.is_none()
                && info.path_translated.as_deref().is_none_or(|p| {
                    std::fs::File::open(std::ffi::OsStr::from_bytes(p)).is_ok()
                }) =>
        {
            // Standard input when no file is named ("Standard input code"):
            // C passes the stream on; `highlight_file()` and
            // `php_strip_whitespace()` take a path, so it goes to a new
            // private file (see `stdin_to_temp`).
            let path = match info.path_translated.clone() {
                Some(p) => p,
                None => match stdin_to_temp() {
                    Some(t) => {
                        stdin_copy = Some(t.clone());
                        t
                    }
                    None => Vec::new(),
                },
            };
            let mut code = if cl.behavior == Behavior::Highlight {
                b"<?php highlight_file('".to_vec()
            } else {
                b"<?php echo php_strip_whitespace('".to_vec()
            };
            for &c in &path {
                if c == b'\'' || c == b'\\' {
                    code.push(b'\\');
                }
                code.push(c);
            }
            code.extend_from_slice(b"');");
            source = Some(code);
        }
        _ => {}
    }

    // `-T`: "gettimeofday(&start, NULL)" when the option is read.
    let mut start = std::time::Instant::now();
    let (mut warmup, mut repeats) = cl.timing.unwrap_or((0, 1));
    let status = loop {
        let vm = run_once(&cfg, &settings, &cl, &info, &environment, source.clone());
        let status = vm.exit_status;
        if cl.timing.is_some() {
            if warmup > 0 {
                warmup -= 1;
                if warmup == 0 {
                    start = std::time::Instant::now();
                }
                drop(vm);
                // Each `-T` run is a request of its own, with a fresh
                // objects store and collector.
                php_vm::reset::release_dropped_vm_objects();
                continue;
            }
            repeats -= 1;
            if repeats > 0 {
                drop(vm);
                php_vm::reset::release_dropped_vm_objects();
                continue;
            }
        }
        // The process exits next: skip the VM's destruction.
        std::mem::forget(vm);
        break status;
    };
    if let Some(t) = stdin_copy {
        let _ = std::fs::remove_file(std::ffi::OsStr::from_bytes(&t));
    }
    if cl.timing.is_some() {
        let e = start.elapsed();
        eprint!(
            "\nElapsed time: {}.{:06} sec\n",
            e.as_secs(),
            e.subsec_micros()
        );
    }
    status
}

/// One request of the command-line or CGI mode (one round of the request
/// loop of `cgi_main.c`): the body, the request, the run, the shutdown.
fn run_once(
    cfg: &Config,
    settings: &env::Settings,
    cl: &CommandLine,
    info: &env::RequestInfo,
    environment: &env::Env,
    source: Option<Vec<u8>>,
) -> php_vm::Vm {
    // The request body: CONTENT_LENGTH bytes of standard input
    // (`sapi_cgi_read_post`).
    let content = if info.content_length > 0 {
        let mut body = Vec::new();
        let n = u64::try_from(info.content_length).unwrap_or(0);
        let _ = std::io::stdin().lock().take(n).read_to_end(&mut body);
        Some(body)
    } else {
        None
    };

    let request = build_request(info, content, environment.vars());
    let php_self = env::php_self(
        info.request_uri.as_deref(),
        environment.get("PATH_INFO").as_deref(),
        settings.fix_pathinfo,
    );
    let cgi_info = CgiInfo {
        no_headers: cl.no_headers,
        no_chdir: cl.no_chdir,
        php_self,
        source,
        ..CgiInfo::default()
    };
    let ctx = Rc::new(RequestCtx::new_cgi(request, cgi_info));
    if info.response_code != 200 {
        let _ = ctx.header_op(php_cli_server::headers::HeaderOp::SetStatus(
            info.response_code,
        ));
    }

    let ini = sapi_ini(cfg);
    let server_cfg = php_cli_server::sapi::ServerConfig {
        document_root: Vec::new(),
        host: String::new(),
        port: 0,
        router: None,
        ini: ini.clone(),
        extensions: extensions::all,
    };
    let mut prepared = new_prepared_vm(cfg, ini);
    if let Some(pt) = &info.path_translated {
        activate_config(
            &mut prepared.vm,
            cfg,
            pt,
            environment.get("SERVER_NAME").as_deref(),
            environment.get("DOCUMENT_ROOT").as_deref(),
        );
    }
    let (_, mut vm) = php_cli_server::sapi::run_request(prepared, &ctx, &server_cfg, false);
    vm.ini_deactivate();
    let _ = std::io::stdout().flush();
    vm.exit_status = ctx.exit_status.get();
    vm
}

/// Standard input copied to a file of its own for `-s`/`-w` without a
/// file: created with `O_CREAT | O_EXCL` and mode 0600 under a fresh name,
/// so a file or symlink someone planted in the temporary directory is
/// never opened (TOCTOU-002: the name used to be predictable and the
/// write followed symlinks).
fn stdin_to_temp() -> Option<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut src = Vec::new();
    std::io::stdin().lock().read_to_end(&mut src).ok()?;
    let dir = std::env::temp_dir();
    for attempt in 0u32..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = dir.join(format!(
            "php-cgi-stdin-{}-{nanos:08x}-{attempt}",
            std::process::id()
        ));
        let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        else {
            continue;
        };
        if f.write_all(&src).is_err() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        return Some(path.into_os_string().into_vec());
    }
    None
}

/// The second option pass: `-h`, `-?` and invalid options print the usage
/// and end the run (with this status).
fn help_pass(argv: &[Vec<u8>], argv0: &[u8], skip_getopt: bool) -> Option<i32> {
    second_pass(argv, argv0, skip_getopt).err()
}

/// The second option pass of `main()`, made in every mode: `-h`, `-?` and
/// invalid options print the usage and end the run (`Err` with the
/// status); `-T [<warmup>,]<count>` asks for timed repeats (`atoi()` of
/// each part). `FastCGI` mode reads `-T` but does not repeat.
fn second_pass(
    argv: &[Vec<u8>],
    argv0: &[u8],
    skip_getopt: bool,
) -> Result<Option<(i64, i64)>, i32> {
    let mut timing = None;
    if skip_getopt {
        return Ok(timing);
    }
    let mut g = GetOpt::new(argv, OPTIONS, true);
    while let Some((c, v)) = g.next_opt() {
        if c == INVALID || c == i32::from(b'h') || c == i32::from(b'?') {
            out(&usage(argv0));
            return Err(i32::from(c == INVALID));
        }
        if c == i32::from(b'T') {
            let arg = v.unwrap_or_default();
            // `atoi(php_optarg)`, as `-T` reads its counts.
            let atoi = |s: &[u8]| i64::from(php_support::atoi(s));
            timing = Some(match arg.iter().position(|&b| b == b',') {
                Some(i) => (atoi(&arg[..i]), atoi(&arg[i + 1..])),
                None => (0, atoi(&arg)),
            });
        }
    }
    Ok(timing)
}

/// The `FastCGI` part of `main()`: `PHP_FCGI_MAX_REQUESTS`,
/// `PHP_FCGI_CHILDREN` (pre-forked children, each replaced when it
/// exits), the second option pass, then requests until the limit, a
/// shutdown signal or a failed `accept()`. Returns the exit status.
fn fastcgi_main(
    cfg: &Config,
    argv: &[Vec<u8>],
    argv0: &[u8],
    skip_getopt: bool,
    listener: fastcgi::Listener,
    allowed: Option<Vec<std::net::IpAddr>>,
) -> i32 {
    let fail = |msg: &str| {
        let _ = std::io::stderr().write_all(msg.as_bytes());
        255
    };
    let mut max_requests: i64 = 500;
    if let Ok(v) = std::env::var("PHP_FCGI_MAX_REQUESTS") {
        max_requests = i64::from(php_support::atoi(v.as_bytes()));
        if max_requests < 0 {
            return fail("PHP_FCGI_MAX_REQUESTS is not valid\n");
        }
    }
    let children_str = std::env::var("PHP_FCGI_CHILDREN").ok();
    let mut children = 0;
    if let Some(c) = &children_str {
        children = i64::from(php_support::atoi(c.as_bytes()));
        if children < 0 {
            return fail("PHP_FCGI_CHILDREN is not valid\n");
        }
    }
    let mgmt = fastcgi::MgmtVars::new(children_str.as_deref());
    if children > 0 {
        // The parent makes the children's VM before it forks them: module
        // startup runs once, and the children share its pages
        // copy-on-write (each takes it for its first request).
        request::adopt_worker_vm(request::prepare_worker_vm(cfg, &[], None));
    }
    if children > 0
        && sys::prefork(u32::try_from(children).unwrap_or(u32::MAX)) == sys::Role::Parent
    {
        return 0;
    }
    if let Some(status) = help_pass(argv, argv0, skip_getopt) {
        return status;
    }

    let settings = request_settings(cfg);
    let logging = cfg_bool(cfg, "fastcgi.logging", true);
    let server = fastcgi::Server::new(listener, allowed, mgmt);
    let mut conn = None;
    let mut requests: i64 = 0;
    let mut exit_status = 0;
    while let Some(params) = server.accept_request(&mut conn) {
        let Some(c) = conn.clone() else {
            break;
        };
        let status = fastcgi_request(
            cfg,
            &settings,
            logging,
            params,
            &c,
            &request::RequestHooks::default(),
        )
        .exit_status;
        if exit_status == 0 {
            exit_status = status;
        }
        requests += 1;
        if max_requests != 0 && requests == max_requests {
            c.finish_request(true);
            if max_requests != 1 {
                exit_status = 0;
            }
            break;
        }
    }
    exit_status
}

/// `-l`: `php_lint_script()`; "No syntax errors detected in <file>" or
/// "Errors parsing <file>" (`Some(255)`). Without a file standard input is
/// linted as "Standard input code"; a file that cannot be opened prints
/// "No input file specified." and ends the run (`None`).
fn lint(cfg: &Config, path: Option<&[u8]>) -> Option<i32> {
    let (src, shown) = if let Some(path) = path {
        let Ok(src) = std::fs::read(std::ffi::OsStr::from_bytes(path)) else {
            out(b"No input file specified.\n");
            return None;
        };
        (src, path.to_vec())
    } else {
        let mut src = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut src);
        (src, b"Standard input code".to_vec())
    };
    let mut vm = new_vm(cfg);
    let ok = vm.lint_script(&src, &ZStr::from(shown.as_slice()));
    let shown = String::from_utf8_lossy(&shown);
    let verdict = if ok {
        format!("No syntax errors detected in {shown}\n")
    } else {
        format!("Errors parsing {shown}\n")
    };
    vm.echo(verdict.as_bytes());
    let _ = vm.shutdown(Ok(()));
    let _ = std::io::stdout().flush();
    std::mem::forget(vm);
    Some(if ok { 0 } else { 255 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_cli_server::parser::Method;
    use request::{ini_bool, method_from};

    fn args(a: &[&str]) -> Vec<Vec<u8>> {
        a.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn command_line_options() {
        let cl = parse_command_line(&args(&["php-cgi", "-C", "-d", "a=1", "x.php", "y"]));
        assert!(cl.no_chdir);
        assert!(!cl.no_headers);
        assert_eq!(cl.optind, 4);
        let cl = parse_command_line(&args(&["php-cgi", "-q", "-f", "x.php"]));
        assert!(cl.no_headers);
        assert_eq!(cl.script_file.as_deref(), Some(&b"x.php"[..]));
        let cl = parse_command_line(&args(&["php-cgi", "-l", "x.php"]));
        assert_eq!(cl.behavior, Behavior::Lint);
        assert!(cl.no_headers);
        let cl = parse_command_line(&args(&["php-cgi", "-n", "-v"]));
        assert_eq!(cl.info, Some(b'v'));
        let cl = parse_command_line(&args(&["php-cgi", "-s", "x.php"]));
        assert_eq!(cl.behavior, Behavior::Highlight);
    }

    #[test]
    fn usage_names_the_program() {
        let u = String::from_utf8(usage(b"/usr/bin/php-cgi8.5")).unwrap();
        assert!(u.starts_with("Usage: php-cgi8.5 [-q] [-h] [-s] [-v] [-i] [-f <file>]\n"));
        assert!(u.ends_with("repeated <count> times.\n"));
    }

    #[test]
    fn version_banner_names_the_sapi() {
        assert!(version_banner().contains(" (cgi-fcgi) (built: "));
    }

    #[test]
    fn methods() {
        assert_eq!(method_from(b"POST"), Method::Post);
        assert_eq!(method_from(b"PUT"), Method::Put);
        assert_eq!(method_from(b""), Method::Get);
        assert_eq!(method_from(b"BREW"), Method::NotImplemented);
    }

    #[test]
    fn ini_booleans() {
        assert!(ini_bool("1"));
        assert!(ini_bool("On"));
        assert!(!ini_bool("0"));
        assert!(!ini_bool("off"));
        assert!(!ini_bool(""));
    }

    #[test]
    fn request_from_environment() {
        let info = env::RequestInfo {
            request_method: Some(b"POST".to_vec()),
            query_string: Some(b"a=1".to_vec()),
            content_type: b"application/x-www-form-urlencoded".to_vec(),
            content_length: 3,
            path_translated: Some(b"/t/x.php".to_vec()),
            ..env::RequestInfo::default()
        };
        let vars = vec![(b"HTTP_COOKIE".to_vec(), b"c=3".to_vec())];
        let r = build_request(&info, Some(b"b=2".to_vec()), &vars);
        assert_eq!(r.header(b"cookie"), Some(&b"c=3"[..]));
        assert_eq!(r.method, Method::Post);
        assert_eq!(
            r.header(b"content-type"),
            Some(&b"application/x-www-form-urlencoded"[..])
        );
        assert_eq!(r.content.as_deref(), Some(&b"b=2"[..]));
        assert_eq!(r.path_translated.as_deref(), Some(&b"/t/x.php"[..]));
    }
}
